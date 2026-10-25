//! libmpv wrapper used by the desktop application.
//!
//! The crate isolates all `unsafe` interaction with libmpv:
//!
//! - `Player` owns the mpv core handle and an event thread. Client API calls
//!   are thread-safe, so commands may be issued from the UI thread.
//! - `RenderContext` owns the OpenGL render context. It must be created,
//!   used and freed on the UI thread while the platform OpenGL context is
//!   current.
//!
//! libmpv is loaded dynamically so a missing `libmpv-2.dll` / `libmpv.so.2`
//! produces an actionable error instead of a loader failure before `main`.

mod sys;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::Instant;

pub const CLIENT_API_VERSION: u64 = 2 << 16;

#[derive(Debug)]
pub enum PlayerError {
    Library(String),
    Mpv(String),
    InvalidArgument(String),
}

impl std::fmt::Display for PlayerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Library(message) => write!(f, "cannot load libmpv: {message}"),
            Self::Mpv(message) => write!(f, "libmpv error: {message}"),
            Self::InvalidArgument(message) => write!(f, "invalid player argument: {message}"),
        }
    }
}

impl std::error::Error for PlayerError {}

pub type Result<T> = std::result::Result<T, PlayerError>;

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    LogMessage {
        level: String,
        prefix: String,
        text: String,
    },
    FileLoaded {
        width: Option<u32>,
        height: Option<u32>,
    },
    VideoReconfig {
        width: Option<u32>,
        height: Option<u32>,
    },
    EndFile {
        eof: bool,
        error: Option<String>,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub id: i64,
    pub kind: TrackKind,
    pub title: String,
    pub language: Option<String>,
    pub codec: Option<String>,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Subtitle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BufferProfile {
    FastStart,
    #[default]
    Standard,
    Large,
    LowMemory,
}

impl BufferProfile {
    pub const ALL: [BufferProfile; 4] = [
        BufferProfile::FastStart,
        BufferProfile::Standard,
        BufferProfile::Large,
        BufferProfile::LowMemory,
    ];

    pub fn id(&self) -> &'static str {
        match self {
            Self::FastStart => "fast_start",
            Self::Standard => "standard",
            Self::Large => "large",
            Self::LowMemory => "low_memory",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::FastStart => "Fast Start",
            Self::Standard => "Standard",
            Self::Large => "Large (4K)",
            Self::LowMemory => "Low-Memory",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::FastStart => "Quick startup with 48 MB buffer & 15s readahead. Best for rapid browsing on high-speed connections.",
            Self::Standard => "Balanced 160 MB buffer & 45s readahead (3.5s resume wait). Smooth 1080p and general 4K streaming.",
            Self::Large => "Deep 384 MB buffer & 90s readahead with 5.5s resume threshold. Resilient against bandwidth dips and 4K seek stalls.",
            Self::LowMemory => "Minimal 24 MB RAM footprint & 8s readahead. Conserves memory on low-spec systems.",
        }
    }

    pub fn demuxer_max_bytes(&self) -> &'static str {
        match self {
            Self::FastStart => "48M",
            Self::Standard => "160M",
            Self::Large => "384M",
            Self::LowMemory => "24M",
        }
    }

    pub fn demuxer_max_back_bytes(&self) -> &'static str {
        match self {
            Self::FastStart => "16M",
            Self::Standard => "48M",
            Self::Large => "96M",
            Self::LowMemory => "8M",
        }
    }

    pub fn demuxer_readahead_secs(&self) -> &'static str {
        match self {
            Self::FastStart => "15",
            Self::Standard => "45",
            Self::Large => "90",
            Self::LowMemory => "8",
        }
    }

    pub fn cache_secs(&self) -> &'static str {
        match self {
            Self::FastStart => "15",
            Self::Standard => "45",
            Self::Large => "90",
            Self::LowMemory => "8",
        }
    }

    pub fn cache_pause_wait(&self) -> &'static str {
        match self {
            Self::FastStart => "1.5",
            Self::Standard => "3.5",
            Self::Large => "5.5",
            Self::LowMemory => "1.5",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_lowercase().as_str() {
            "fast_start" | "fast-start" | "faststart" => Some(Self::FastStart),
            "standard" => Some(Self::Standard),
            "large" | "4k" => Some(Self::Large),
            "low_memory" | "low-memory" | "lowmemory" | "low_ram" => Some(Self::LowMemory),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PlaybackStats {
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub decoder: Option<String>,
    pub hwdec: Option<String>,
    pub dropped_frames: Option<u64>,
    pub container_fps: Option<f64>,
    pub video_bitrate: Option<f64>,
    pub audio_bitrate: Option<f64>,
    pub file_format: Option<String>,
    pub color_primaries: Option<String>,
    pub gamma: Option<String>,
    pub pixelformat: Option<String>,
    pub vo: Option<String>,
    pub cache_duration_secs: Option<f64>,
    pub cache_forward_bytes: Option<u64>,
    pub cache_total_bytes: Option<u64>,
    pub raw_input_rate_bps: Option<f64>,
    pub paused_for_cache: bool,
    pub buffer_profile_name: Option<String>,
    pub buffer_profile_details: Option<String>,
    pub stall_count: u32,
    pub total_stalled_duration_secs: f64,
    pub last_stall_diagnosis: Option<String>,
    pub file_size: Option<u64>,
    pub quality_variant: Option<String>,
}

pub fn format_file_size(bytes: u64) -> String {
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

pub fn sanitize_media_url(raw: &str) -> String {
    let without_query = match raw.find('?') {
        Some(idx) => &raw[..idx],
        None => raw,
    };
    let without_hash = match without_query.find('#') {
        Some(idx) => &without_query[..idx],
        None => without_query,
    };
    if let Some(pos) = without_hash.find("://") {
        let scheme = &without_hash[..pos + 3];
        let rest = &without_hash[pos + 3..];
        if let Some(at_idx) = rest.find('@') {
            format!("{scheme}{}", &rest[at_idx + 1..])
        } else {
            without_hash.to_string()
        }
    } else {
        without_hash.to_string()
    }
}

impl PlaybackStats {
    pub fn to_hud_text(&self) -> String {
        let mut lines = Vec::new();
        lines.push("[ Stream & Media ]".to_string());

        let res = match (self.width, self.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => format!("{w}x{h}"),
            _ => "Unknown".to_string(),
        };
        lines.push(format!("Resolution: {res}"));

        if let Some(ref q) = self.quality_variant {
            lines.push(format!("Quality Variant: {q}"));
        }

        let vcodec = self.video_codec.as_deref().unwrap_or("Unknown");
        lines.push(format!("Video Codec: {vcodec}"));

        let acodec = self.audio_codec.as_deref().unwrap_or("Unknown");
        lines.push(format!("Audio Codec: {acodec}"));

        let decoder = self.decoder.as_deref().unwrap_or("Unknown");
        let hwdec = self.hwdec.as_deref().unwrap_or("no");
        let hw_label = if hwdec != "no" && !hwdec.is_empty() {
            format!("{decoder} (HW: {hwdec})")
        } else {
            format!("{decoder} (Software Fallback)")
        };
        lines.push(format!("Decoder: {hw_label}"));

        if let Some(bytes) = self.file_size {
            lines.push(format!("File size: {}", format_file_size(bytes)));
        } else {
            lines.push("File size: Unavailable".to_string());
        }

        if let Some(format) = &self.file_format {
            lines.push(format!("Container: {format}"));
        }

        if let Some(pixelformat) = &self.pixelformat {
            lines.push(format!("Pixel format: {pixelformat}"));
        }

        lines.push("".to_string());
        lines.push("[ Playback & Buffer ]".to_string());

        if let Some(profile_name) = &self.buffer_profile_name {
            let details = self.buffer_profile_details.as_deref().unwrap_or("");
            lines.push(format!("Buffer Profile: {profile_name} ({details})"));
        }

        if let Some(cache_secs) = self.cache_duration_secs {
            let state = if self.paused_for_cache {
                " (Buffering...)"
            } else {
                ""
            };
            let fw_str = self
                .cache_forward_bytes
                .map(|b| format!(" / {}", format_file_size(b)))
                .unwrap_or_default();
            lines.push(format!("Buffer Cache: {cache_secs:.1} s{fw_str}{state}"));
        } else {
            lines.push("Buffer Cache: N/A".to_string());
        }

        if let Some(rate_bps) = self.raw_input_rate_bps {
            if rate_bps > 0.0 {
                let mbps = (rate_bps * 8.0) / 1_000_000.0;
                let mbs = rate_bps / (1024.0 * 1024.0);
                lines.push(format!("Download Rate: {mbps:.2} Mbit/s ({mbs:.2} MB/s)"));
            } else {
                lines.push("Download Rate: Idle / Cached".to_string());
            }
        } else {
            lines.push("Download Rate: N/A".to_string());
        }

        if let Some(fps) = self.container_fps {
            lines.push(format!("Container FPS: {fps:.2}"));
        } else {
            lines.push("Container FPS: N/A".to_string());
        }

        if let Some(bitrate) = self.video_bitrate {
            if bitrate > 0.0 {
                let mbps = bitrate / 1_000_000.0;
                lines.push(format!("Video Bitrate: {mbps:.2} Mbit/s ({:.0} kbit/s)", bitrate / 1000.0));
            } else {
                lines.push("Video Bitrate: 0 kbit/s (Paused)".to_string());
            }
        } else {
            lines.push("Video Bitrate: N/A".to_string());
        }

        let dropped = self.dropped_frames.unwrap_or(0);
        lines.push(format!("Dropped Frames: {dropped}"));

        if self.stall_count > 0 {
            let last_diag = self.last_stall_diagnosis.as_deref().unwrap_or("Buffer starved");
            lines.push(format!(
                "Stalls: {} ({:.1} s total) - {last_diag}",
                self.stall_count, self.total_stalled_duration_secs
            ));
        }

        if let (Some(primaries), Some(gamma)) = (&self.color_primaries, &self.gamma) {
            lines.push(format!("Color: {primaries} / {gamma} (SDR mapped)"));
        }

        if let Some(vo) = &self.vo {
            lines.push(format!("Video Output: {vo}"));
        }

        if let Some(format) = &self.file_format {
            lines.push(format!("Container: {format}"));
        }
        lines.join("\n")
    }
}

#[derive(Debug, Clone, Default)]
pub struct StallSummary {
    pub stall_count: u32,
    pub total_stalled_duration_secs: f64,
    pub last_stall_diagnosis: Option<String>,
}

pub struct Player {
    api: Arc<sys::MpvApi>,
    handle: *mut sys::mpv_handle,
    events: Receiver<PlayerEvent>,
    event_thread: Option<JoinHandle<()>>,
    quit: Arc<AtomicBool>,
    last_load: Mutex<Option<LoadRequest>>,
    load_generation: AtomicU64,
    active_profile: Mutex<BufferProfile>,
    last_seek: Mutex<Option<Instant>>,
    last_load_time: Mutex<Option<Instant>>,
    stall_summary: Mutex<StallSummary>,
    transfer_sampler: Mutex<Option<(Instant, u64, Option<f64>)>>,
    current_quality: Mutex<Option<String>>,
}

// SAFETY: libmpv client API is documented as safe to call from multiple threads.
// All client API calls on the same mpv_handle are synchronized internally by libmpv.
unsafe impl Send for Player {}
unsafe impl Sync for Player {}

/// The raw libmpv handle is safe to use from the event thread: libmpv
/// documents that calls on the same handle from different threads are safe.
struct SendHandle(*mut sys::mpv_handle);
unsafe impl Send for SendHandle {}

impl Player {
    /// Creates a player whose video is rendered into an embedded OpenGL texture
    /// via the libmpv render API.
    pub fn new() -> Result<Self> {
        Self::with_video_output(Some("libmpv"), Some("opengl"))
    }

    /// Creates a player that opens its own top-level video window. Used when
    /// the application UI cannot provide a shared OpenGL context, so video
    /// rendering is fully independent of the UI renderer. Leaving `vo` at the
    /// mpv default keeps its own fallback chain (GPU context -> legacy VOs)
    /// intact; newer mpv releases no longer accept an explicit "auto" value.
    pub fn new_native_video() -> Result<Self> {
        Self::with_video_output(None, None)
    }

    fn with_video_output(vo: Option<&str>, gpu_api: Option<&str>) -> Result<Self> {
        let api = Arc::new(sys::MpvApi::load()?);
        let handle = unsafe { (api.mpv_create)() };
        if handle.is_null() {
            return Err(PlayerError::Mpv("mpv_create returned null".into()));
        }

        // Isolated playback: never inherit the user's mpv configuration,
        // scripts or key bindings.
        let hwdec_mode = std::env::var("ADULT_HUB_HWDEC").unwrap_or_else(|_| "auto-copy".into());
        let ao_mode = std::env::var("ADULT_HUB_AO").ok();
        let ao_pcm_file = std::env::var("ADULT_HUB_AO_PCM_FILE").ok();
        let initial_profile = BufferProfile::Standard;
        let mut options: Vec<(&str, &str)> = vec![
            ("config", "no"),
            ("load-scripts", "no"),
            ("input-default-bindings", "no"),
            ("osc", "no"),
            ("terminal", "no"),
            ("idle", "yes"),
            ("hwdec", hwdec_mode.as_str()),
            ("audio-fallback-to-null", "yes"),
            ("keep-open", "no"),
            ("audio-client-name", "Adult Hub Desktop"),
            ("title", "Adult Hub Desktop"),
            ("msg-level", "all=warn"),
            ("tone-mapping", "auto"),
            ("target-colorspace-hint", "no"),
            ("target-trc", "srgb"),
            ("cache", "yes"),
            ("cache-pause", "yes"),
            ("cache-pause-initial", "no"),
            ("cache-pause-wait", initial_profile.cache_pause_wait()),
            ("demuxer-max-bytes", initial_profile.demuxer_max_bytes()),
            ("demuxer-max-back-bytes", initial_profile.demuxer_max_back_bytes()),
            ("demuxer-readahead-secs", initial_profile.demuxer_readahead_secs()),
            ("cache-secs", initial_profile.cache_secs()),
            ("force-seekable", "yes"),
            ("vd-lavc-threads", "0"),
            ("hr-seek", "yes"),
            ("video-sync", "audio"),
        ];
        if let Some(vo) = vo {
            options.push(("vo", vo));
        }
        if let Some(api_name) = gpu_api {
            options.push(("gpu-api", api_name));
        }
        for (name, value) in options {
            if let Err(error) = unsafe { set_option_raw(&api, handle, name, value) } {
                unsafe { (api.mpv_terminate_destroy)(handle) };
                return Err(error);
            }
        }

        if let Some(ao) = &ao_mode {
            if let Err(error) = unsafe { set_option_raw(&api, handle, "ao", ao.as_str()) } {
                unsafe { (api.mpv_terminate_destroy)(handle) };
                return Err(error);
            }
        }
        if let Some(pcm_file) = &ao_pcm_file {
            if let Err(error) =
                unsafe { set_option_raw(&api, handle, "ao-pcm-file", pcm_file.as_str()) }
            {
                unsafe { (api.mpv_terminate_destroy)(handle) };
                return Err(error);
            }
        }

        let status = unsafe { (api.mpv_initialize)(handle) };
        if status < 0 {
            let message = unsafe { error_string(&api, status) };
            unsafe { (api.mpv_terminate_destroy)(handle) };
            return Err(PlayerError::Mpv(message));
        }

        let log_level = CString::new("warn").expect("static log level has no NUL bytes");
        unsafe {
            (api.mpv_request_log_messages)(handle, log_level.as_ptr());
        }

        let (sender, events) = mpsc::channel();
        let quit = Arc::new(AtomicBool::new(false));
        let event_thread = {
            let api = Arc::clone(&api);
            let quit = Arc::clone(&quit);
            let handle = SendHandle(handle);
            std::thread::Builder::new()
                .name("mpv-events".into())
                .spawn(move || event_loop(api, handle, sender, quit))
                .map_err(|error| PlayerError::Mpv(format!("cannot start event thread: {error}")))?
        };

        Ok(Self {
            api,
            handle,
            events,
            event_thread: Some(event_thread),
            quit,
            last_load: Mutex::new(None),
            load_generation: AtomicU64::new(0),
            active_profile: Mutex::new(initial_profile),
            last_seek: Mutex::new(None),
            last_load_time: Mutex::new(None),
            stall_summary: Mutex::new(StallSummary::default()),
            transfer_sampler: Mutex::new(None),
            current_quality: Mutex::new(None),
        })
    }

    pub fn events(&self) -> &Receiver<PlayerEvent> {
        &self.events
    }

    pub fn load(&self, url: &str, headers: &[String]) -> Result<()> {
        self.load_with_start(url, headers, None)
    }

    pub fn load_with_start(
        &self,
        url: &str,
        headers: &[String],
        start_position: Option<f64>,
    ) -> Result<()> {
        if url.trim().is_empty() {
            return Err(PlayerError::InvalidArgument("empty media URL".into()));
        }
        let request = LoadRequest {
            url: url.to_owned(),
            headers: headers.to_vec(),
            start_position,
        };
        self.issue_load(&request)?;
        *self.last_load.lock().unwrap() = Some(request);
        self.load_generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub fn load_generation(&self) -> u64 {
        self.load_generation.load(Ordering::SeqCst)
    }

    /// Reload the current source with its original headers, resuming at the
    /// supplied position. This deliberately does not replace the remembered
    /// source request, so repeated retries keep the same URL and credentials.
    pub fn retry_current_at(&self, position_seconds: f64) -> Result<()> {
        let mut request = self
            .last_load
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| PlayerError::InvalidArgument("no media source to retry".into()))?;
        request.start_position = Some(position_seconds.max(0.0));
        self.issue_load(&request)
    }

    pub fn is_mpv_at_least_0_38(&self) -> bool {
        let version_str = self.get_string("mpv-version").unwrap_or_default();
        let s = version_str
            .trim()
            .strip_prefix("mpv ")
            .unwrap_or(version_str.trim());
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.');
        if let (Some(major_str), Some(minor_str)) = (parts.next(), parts.next()) {
            if let (Ok(major), Ok(minor)) = (
                major_str.parse::<u32>(),
                minor_str
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .unwrap_or("0")
                    .parse::<u32>(),
            ) {
                return major > 0 || minor >= 38;
            }
        }
        true
    }

    fn issue_load(&self, request: &LoadRequest) -> Result<()> {
        if request.url.trim().is_empty() {
            return Err(PlayerError::InvalidArgument("empty media URL".into()));
        }
        *self.last_load_time.lock().unwrap() = Some(Instant::now());
        self.reset_stall_stats();
        *self.transfer_sampler.lock().unwrap() = None;
        let active_prof = *self.active_profile.lock().unwrap();
        let _ = self.set_buffer_profile(active_prof);
        if !request.headers.is_empty() {
            if let Err(e) =
                self.set_property_string("http-header-fields", &request.headers.join(","))
            {
                return Err(PlayerError::Mpv(format!(
                    "operation: set http headers, stage: mpv_set_property_string, detail: {e}"
                )));
            }
        }
        let _ = self.set_pause(false);
        if let Some(seconds) = request.start_position {
            if seconds > 0.0 {
                let _ = self.set_property_string("start", &format!("{seconds:.3}"));
            } else {
                let _ = self.set_property_string("start", "none");
            }
        } else {
            let _ = self.set_property_string("start", "none");
        }
        let result = match request.start_position {
            Some(seconds) if seconds > 0.0 => {
                let start_opt = format!("start={seconds:.3}");
                if self.is_mpv_at_least_0_38() {
                    match self.command(&["loadfile", &request.url, "replace", "-1", &start_opt]) {
                        Ok(()) => Ok(()),
                        Err(_) => self.command(&["loadfile", &request.url, "replace", &start_opt]),
                    }
                } else {
                    match self.command(&["loadfile", &request.url, "replace", &start_opt]) {
                        Ok(()) => Ok(()),
                        Err(_) => {
                            self.command(&["loadfile", &request.url, "replace", "-1", &start_opt])
                        }
                    }
                }
            }
            _ => self.command(&["loadfile", &request.url, "replace"]),
        };
        result.map_err(|e| {
            let sanitized_url = sanitize_media_url(&request.url);
            let has_start = request
                .start_position
                .map(|s| format!("start={s:.3}s"))
                .unwrap_or_else(|| "none".into());
            PlayerError::Mpv(format!(
                "operation: load media stream, stage: mpv loadfile command, start_pos: {has_start}, target: {sanitized_url}, detail: {e}"
            ))
        })
    }

    pub fn stop(&self) -> Result<()> {
        self.command(&["stop"])
    }

    pub fn set_pause(&self, paused: bool) -> Result<()> {
        self.set_property_string("pause", if paused { "yes" } else { "no" })
    }

    pub fn is_paused(&self) -> bool {
        self.get_string("pause").as_deref() == Some("yes")
    }

    pub fn toggle_pause(&self) -> Result<bool> {
        let paused = !self.is_paused();
        self.set_pause(paused)?;
        Ok(paused)
    }

    pub fn seek_absolute(&self, seconds: f64) -> Result<()> {
        *self.last_seek.lock().unwrap() = Some(Instant::now());
        self.set_property_string("time-pos", &format!("{seconds:.3}"))
    }

    pub fn seek_relative(&self, seconds: f64) -> Result<()> {
        *self.last_seek.lock().unwrap() = Some(Instant::now());
        self.command(&["seek", &format!("{seconds:.3}"), "relative"])
    }

    pub fn last_seek_time(&self) -> Option<Instant> {
        *self.last_seek.lock().unwrap()
    }

    pub fn last_load_time(&self) -> Option<Instant> {
        *self.last_load_time.lock().unwrap()
    }

    pub fn update_stall_summary(&self, count: u32, total_dur: f64, diag: Option<String>) {
        let mut s = self.stall_summary.lock().unwrap();
        s.stall_count = count;
        s.total_stalled_duration_secs = total_dur;
        s.last_stall_diagnosis = diag;
    }

    pub fn reset_stall_stats(&self) {
        let mut s = self.stall_summary.lock().unwrap();
        *s = StallSummary::default();
    }

    pub fn position(&self) -> Option<f64> {
        self.get_f64("time-pos")
    }

    pub fn duration(&self) -> Option<f64> {
        self.get_f64("duration")
    }

    pub fn volume(&self) -> Option<f64> {
        self.get_f64("volume")
    }

    pub fn set_volume(&self, volume: f64) -> Result<()> {
        self.set_property_string("volume", &format!("{:.1}", volume.clamp(0.0, 130.0)))
    }

    pub fn is_muted(&self) -> bool {
        self.get_string("mute").as_deref() == Some("yes")
    }

    pub fn set_muted(&self, muted: bool) -> Result<()> {
        self.set_property_string("mute", if muted { "yes" } else { "no" })
    }

    pub fn set_speed(&self, speed: f64) -> Result<()> {
        self.set_property_string("speed", &format!("{:.2}", speed.clamp(0.25, 4.0)))
    }

    /// `None` restores automatic aspect handling.
    pub fn set_aspect_override(&self, aspect: Option<&str>) -> Result<()> {
        match aspect {
            Some(value) => self.set_property_string("video-aspect-override", value),
            None => self.set_property_string("video-aspect-override", "no"),
        }
    }

    pub fn tracks(&self) -> Vec<Track> {
        let Some(raw) = self.get_string("track-list") else {
            return Vec::new();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Vec::new();
        };
        let Some(items) = value.as_array() else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|item| {
                let kind = match item.get("type")?.as_str()? {
                    "audio" => TrackKind::Audio,
                    "sub" => TrackKind::Subtitle,
                    _ => return None,
                };
                Some(Track {
                    id: item.get("id")?.as_i64()?,
                    kind,
                    title: item
                        .get("title")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    language: item
                        .get("lang")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned),
                    codec: item
                        .get("codec")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned),
                    selected: item
                        .get("selected")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                })
            })
            .collect()
    }

    pub fn set_audio_track(&self, id: Option<i64>) -> Result<()> {
        self.set_property_string(
            "aid",
            &id.map(|id| id.to_string()).unwrap_or_else(|| "no".into()),
        )
    }

    pub fn set_subtitle_track(&self, id: Option<i64>) -> Result<()> {
        self.set_property_string(
            "sid",
            &id.map(|id| id.to_string()).unwrap_or_else(|| "no".into()),
        )
    }

    pub fn set_buffer_profile(&self, profile: BufferProfile) -> Result<()> {
        self.set_property_string("cache", "yes")?;
        self.set_property_string("cache-pause", "yes")?;
        self.set_property_string("cache-pause-wait", profile.cache_pause_wait())?;
        self.set_property_string("demuxer-max-bytes", profile.demuxer_max_bytes())?;
        self.set_property_string("demuxer-max-back-bytes", profile.demuxer_max_back_bytes())?;
        self.set_property_string("demuxer-readahead-secs", profile.demuxer_readahead_secs())?;
        self.set_property_string("cache-secs", profile.cache_secs())?;
        *self.active_profile.lock().unwrap() = profile;
        Ok(())
    }

    pub fn buffer_profile(&self) -> BufferProfile {
        *self.active_profile.lock().unwrap()
    }

    pub fn effective_buffer_params(&self) -> (String, String, String, String, String) {
        let active = self.buffer_profile();
        (
            self.get_string("demuxer-max-bytes").unwrap_or_else(|| active.demuxer_max_bytes().into()),
            self.get_string("demuxer-max-back-bytes").unwrap_or_else(|| active.demuxer_max_back_bytes().into()),
            self.get_string("demuxer-readahead-secs").unwrap_or_else(|| active.demuxer_readahead_secs().into()),
            self.get_string("cache-secs").unwrap_or_else(|| active.cache_secs().into()),
            self.get_string("cache-pause-wait").unwrap_or_else(|| active.cache_pause_wait().into()),
        )
    }

    pub fn set_hwdec(&self, hwdec: &str) -> Result<()> {
        self.set_property_string("hwdec", hwdec)
    }

    pub fn current_hwdec(&self) -> Option<String> {
        self.get_string("hwdec-current")
            .filter(|s| s != "no")
            .or_else(|| self.get_string("hwdec").filter(|s| s != "no"))
    }

    /// Reports whether the bundled libmpv build contains common video decoders.
    /// This is codec availability, not a probe of the machine's GPU capabilities.
    pub fn common_video_decoder_support(&self) -> Option<[bool; 4]> {
        const CODECS: [&[u8]; 4] = [b"h264", b"hevc", b"vp9", b"av1"];
        let property = CString::new("decoder-list").ok()?;
        let mut node: sys::mpv_node = unsafe { std::mem::zeroed() };
        let status = unsafe {
            (self.api.mpv_get_property)(
                self.handle,
                property.as_ptr(),
                sys::MPV_FORMAT_NODE,
                (&mut node as *mut sys::mpv_node).cast(),
            )
        };
        if status < 0 {
            return None;
        }

        let support = unsafe {
            if node.format != sys::MPV_FORMAT_NODE_ARRAY || node.u.list.is_null() {
                None
            } else {
                let list = &*node.u.list;
                if list.num < 0 || (list.num > 0 && list.values.is_null()) {
                    None
                } else {
                    let mut support = [false; 4];
                    for index in 0..(list.num as usize).min(4096) {
                        let entry = &*list.values.add(index);
                        if entry.format != sys::MPV_FORMAT_NODE_MAP || entry.u.list.is_null() {
                            continue;
                        }

                        let map = &*entry.u.list;
                        if map.num < 0
                            || (map.num > 0 && (map.values.is_null() || map.keys.is_null()))
                        {
                            continue;
                        }

                        let mut codec_name: Option<&[u8]> = None;
                        for field_index in 0..(map.num as usize).min(64) {
                            let key = *map.keys.add(field_index);
                            let value = &*map.values.add(field_index);
                            if key.is_null()
                                || CStr::from_ptr(key).to_bytes() != b"codec"
                                || value.format != sys::MPV_FORMAT_STRING
                                || value.u.string.is_null()
                            {
                                continue;
                            }
                            codec_name = Some(CStr::from_ptr(value.u.string).to_bytes());
                            break;
                        }

                        if let Some(codec_name) = codec_name {
                            for (codec_index, codec) in CODECS.iter().enumerate() {
                                support[codec_index] |= codec_name.eq_ignore_ascii_case(codec);
                            }
                        }
                    }
                    Some(support)
                }
            }
        };

        unsafe { (self.api.mpv_free_node_contents)(&mut node) };
        support
    }

    pub fn stats(&self) -> PlaybackStats {
        let width = self
            .get_u32("width")
            .filter(|w| *w > 0)
            .or_else(|| self.get_u32("dwidth").filter(|w| *w > 0))
            .or_else(|| self.get_u32("video-params/w").filter(|w| *w > 0));
        let height = self
            .get_u32("height")
            .filter(|h| *h > 0)
            .or_else(|| self.get_u32("dheight").filter(|h| *h > 0))
            .or_else(|| self.get_u32("video-params/h").filter(|h| *h > 0));
        let cache_duration_secs = self.get_f64("demuxer-cache-duration");
        let paused_for_cache = self.get_string("paused-for-cache").as_deref() == Some("yes");

        let video_codec = self
            .get_string("video-codec")
            .or_else(|| self.get_string("video-format"));
        let audio_codec = self
            .get_string("audio-codec-name")
            .or_else(|| self.get_string("audio-codec"))
            .or_else(|| self.get_string("audio-format"));

        let cache_forward_bytes = self.get_u64("demuxer-cache-state/fw-bytes");
        let cache_total_bytes = self.get_u64("demuxer-cache-state/total-bytes");
        let raw_input_rate_bps = self.get_f64("cache-speed")
            .filter(|&r| r > 0.0)
            .or_else(|| self.get_f64("demuxer-cache-state/raw-input-rate").filter(|&r| r > 0.0))
            .or_else(|| {
                if let Some(total) = cache_total_bytes {
                    let now = Instant::now();
                    let mut sampler = self.transfer_sampler.lock().unwrap();
                    if let Some((last_time, last_bytes, prev_speed)) = *sampler {
                        let elapsed = now.saturating_duration_since(last_time).as_secs_f64();
                        if elapsed >= 0.8 {
                            let speed = if total >= last_bytes {
                                let delta = (total - last_bytes) as f64;
                                let rate = delta / elapsed;
                                if rate > 0.0 { Some(rate) } else { None }
                            } else {
                                None
                            };
                            *sampler = Some((now, total, speed));
                            speed
                        } else {
                            prev_speed
                        }
                    } else {
                        *sampler = Some((now, total, None));
                        None
                    }
                } else {
                    None
                }
            });
        let audio_bitrate = self.get_f64("audio-bitrate");
        let active_prof = self.buffer_profile();

        let stall = self.stall_summary.lock().unwrap().clone();

        PlaybackStats {
            video_codec: video_codec.clone(),
            audio_codec,
            width,
            height,
            decoder: self.get_string("current-decoder").or(video_codec),
            hwdec: self.current_hwdec(),
            dropped_frames: self
                .get_string("decoder-frame-drop-count")
                .and_then(|value| value.parse().ok()),
            container_fps: self.get_f64("container-fps"),
            video_bitrate: self.get_f64("video-bitrate"),
            audio_bitrate,
            file_format: self.get_string("file-format"),
            color_primaries: self.get_string("video-params/primaries"),
            gamma: self.get_string("video-params/gamma"),
            pixelformat: self.get_string("video-params/pixelformat"),
            vo: self.get_string("current-vo"),
            cache_duration_secs,
            cache_forward_bytes,
            cache_total_bytes,
            raw_input_rate_bps,
            paused_for_cache,
            buffer_profile_name: Some(active_prof.name().to_string()),
            buffer_profile_details: Some(format!(
                "{} / {}s, wait: {}s",
                active_prof.demuxer_max_bytes(),
                active_prof.demuxer_readahead_secs(),
                active_prof.cache_pause_wait()
            )),
            stall_count: stall.stall_count,
            total_stalled_duration_secs: stall.total_stalled_duration_secs,
            last_stall_diagnosis: stall.last_stall_diagnosis,
            file_size: self.get_u64("file-size"),
            quality_variant: self.current_quality.lock().unwrap().clone(),
        }
    }

    pub fn quality_variant(&self) -> Option<String> {
        self.current_quality.lock().unwrap().clone()
    }

    pub fn set_quality_variant(&self, variant: Option<String>) {
        *self.current_quality.lock().unwrap() = variant;
    }

    pub fn get_string(&self, name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        let raw = unsafe { (self.api.mpv_get_property_string)(self.handle, name.as_ptr()) };
        if raw.is_null() {
            return None;
        }
        let value = unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned();
        unsafe { (self.api.mpv_free)(raw.cast()) };
        if value.is_empty() { None } else { Some(value) }
    }

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        self.get_string(name)?.parse().ok()
    }

    pub fn get_u32(&self, name: &str) -> Option<u32> {
        self.get_string(name)?.parse().ok()
    }

    pub fn get_u64(&self, name: &str) -> Option<u64> {
        self.get_string(name)?.parse().ok()
    }

    pub fn set_property_string(&self, name: &str, value: &str) -> Result<()> {
        let name = CString::new(name)
            .map_err(|_| PlayerError::InvalidArgument("property name contains NUL".into()))?;
        let value = CString::new(value)
            .map_err(|_| PlayerError::InvalidArgument("property value contains NUL".into()))?;
        let status = unsafe {
            (self.api.mpv_set_property_string)(self.handle, name.as_ptr(), value.as_ptr())
        };
        if status < 0 {
            return Err(PlayerError::Mpv(unsafe { error_string(&self.api, status) }));
        }
        Ok(())
    }

    pub fn command(&self, args: &[&str]) -> Result<()> {
        let owned = args
            .iter()
            .map(|arg| CString::new(*arg))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| PlayerError::InvalidArgument("command argument contains NUL".into()))?;
        let mut pointers = owned.iter().map(|arg| arg.as_ptr()).collect::<Vec<_>>();
        pointers.push(std::ptr::null());
        let status = unsafe { (self.api.mpv_command)(self.handle, pointers.as_ptr()) };
        if status < 0 {
            return Err(PlayerError::Mpv(unsafe { error_string(&self.api, status) }));
        }
        Ok(())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Release);
        unsafe { (self.api.mpv_terminate_destroy)(self.handle) };
        if let Some(thread) = self.event_thread.take() {
            let _ = thread.join();
        }
    }
}

unsafe fn set_option_raw(
    api: &sys::MpvApi,
    handle: *mut sys::mpv_handle,
    name: &str,
    value: &str,
) -> Result<()> {
    let name = CString::new(name)
        .map_err(|_| PlayerError::InvalidArgument("option name contains NUL".into()))?;
    let value = CString::new(value)
        .map_err(|_| PlayerError::InvalidArgument("option value contains NUL".into()))?;
    let status = unsafe { (api.mpv_set_option_string)(handle, name.as_ptr(), value.as_ptr()) };
    if status < 0 {
        return Err(PlayerError::Mpv(unsafe { error_string(api, status) }));
    }
    Ok(())
}

unsafe fn error_string(api: &sys::MpvApi, status: c_int) -> String {
    let raw = unsafe { (api.mpv_error_string)(status) };
    if raw.is_null() {
        return format!("error code {status}");
    }
    unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned()
}

fn event_loop(
    api: Arc<sys::MpvApi>,
    handle: SendHandle,
    sender: mpsc::Sender<PlayerEvent>,
    quit: Arc<AtomicBool>,
) {
    let handle = handle.0;
    while !quit.load(Ordering::Acquire) {
        let event = unsafe { (api.mpv_wait_event)(handle, 0.25) };
        if event.is_null() {
            continue;
        }
        let event_id = unsafe { (*event).event_id };
        match event_id {
            sys::MPV_EVENT_NONE => {}
            sys::MPV_EVENT_LOG_MESSAGE => {
                let data = unsafe { (*event).data.cast::<sys::mpv_event_log_message>() };
                if data.is_null() {
                    continue;
                }
                let message = unsafe { &*data };
                let copy = |value: *const c_char| {
                    if value.is_null() {
                        String::new()
                    } else {
                        unsafe { CStr::from_ptr(value) }
                            .to_string_lossy()
                            .trim()
                            .to_owned()
                    }
                };
                let _ = sender.send(PlayerEvent::LogMessage {
                    level: copy(message.level),
                    prefix: copy(message.prefix),
                    text: copy(message.text),
                });
            }
            sys::MPV_EVENT_SHUTDOWN => {
                let _ = sender.send(PlayerEvent::Shutdown);
                break;
            }
            sys::MPV_EVENT_FILE_LOADED => {
                let dwidth = get_u32_handle(&api, handle, "dwidth");
                let dheight = get_u32_handle(&api, handle, "dheight");
                let width = get_u32_handle(&api, handle, "width");
                let height = get_u32_handle(&api, handle, "height");
                let _ = sender.send(PlayerEvent::FileLoaded {
                    width: dwidth.or(width),
                    height: dheight.or(height),
                });
            }
            sys::MPV_EVENT_VIDEO_RECONFIG | sys::MPV_EVENT_PLAYBACK_RESTART => {
                let dwidth = get_u32_handle(&api, handle, "dwidth");
                let dheight = get_u32_handle(&api, handle, "dheight");
                let width = get_u32_handle(&api, handle, "width");
                let height = get_u32_handle(&api, handle, "height");
                let _ = sender.send(PlayerEvent::VideoReconfig {
                    width: dwidth.or(width),
                    height: dheight.or(height),
                });
            }
            sys::MPV_EVENT_END_FILE => {
                let data = unsafe { (*event).data.cast::<sys::mpv_event_end_file>() };
                if data.is_null() {
                    continue;
                }
                let reason = unsafe { (*data).reason };
                let error_code = unsafe { (*data).error };
                let error = if reason == sys::MPV_END_FILE_REASON_ERROR && error_code < 0 {
                    Some(unsafe { error_string(&api, error_code) })
                } else {
                    None
                };
                let _ = sender.send(PlayerEvent::EndFile {
                    eof: reason == sys::MPV_END_FILE_REASON_EOF,
                    error,
                });
            }
            _ => {}
        }
    }
}

fn get_u32_handle(api: &sys::MpvApi, handle: *mut sys::mpv_handle, name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    let raw = unsafe { (api.mpv_get_property_string)(handle, name.as_ptr()) };
    if raw.is_null() {
        return None;
    }
    let value = unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned();
    unsafe { (api.mpv_free)(raw.cast()) };
    value.parse().ok()
}

/// OpenGL render context. Use only while the Slint renderer's GL context is
/// current, i.e. inside the rendering notifier callbacks.
pub struct RenderContext {
    api: Arc<sys::MpvApi>,
    context: *mut sys::mpv_render_context,
    flip_y: bool,
    update_callback: Option<Box<UpdateCallback>>,
    _player: Arc<Player>,
}

struct UpdateCallback(Box<dyn Fn() + Send>);

pub type ProcAddressFn<'a> = dyn Fn(&CStr) -> *const c_void + 'a;

impl RenderContext {
    /// Must be called during `RenderingSetup` with the OpenGL context current.
    pub fn new(
        player: &Arc<Player>,
        get_proc_address: &ProcAddressFn<'_>,
        flip_y: bool,
    ) -> Result<Self> {
        struct Resolver<'a> {
            function: &'a ProcAddressFn<'a>,
        }

        unsafe extern "C" fn resolve(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
            let resolver = unsafe { &*(ctx.cast::<Resolver<'_>>()) };
            (resolver.function)(unsafe { CStr::from_ptr(name) }).cast_mut()
        }

        let mut resolver = Resolver {
            function: get_proc_address,
        };
        let mut init_params = sys::mpv_opengl_init_params {
            get_proc_address: Some(resolve),
            get_proc_address_ctx: (&mut resolver as *mut Resolver<'_>).cast(),
        };
        let mut params = [
            sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_API_TYPE,
                data: sys::MPV_RENDER_API_TYPE_OPENGL.as_ptr().cast_mut().cast(),
            },
            sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: (&mut init_params as *mut sys::mpv_opengl_init_params).cast(),
            },
            sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let mut context = std::ptr::null_mut();
        let status = unsafe {
            (player.api.mpv_render_context_create)(&mut context, player.handle, params.as_mut_ptr())
        };
        if status < 0 {
            return Err(PlayerError::Mpv(unsafe {
                error_string(&player.api, status)
            }));
        }

        Ok(Self {
            api: Arc::clone(&player.api),
            context,
            flip_y,
            update_callback: None,
            _player: Arc::clone(player),
        })
    }

    pub fn set_update_callback(&mut self, callback: impl Fn() + Send + 'static) {
        // Replace any previous callback only after clearing it in libmpv.
        unsafe {
            (self.api.mpv_render_context_set_update_callback)(
                self.context,
                None,
                std::ptr::null_mut(),
            )
        };
        let mut boxed = Box::new(UpdateCallback(Box::new(callback)));
        let data = (&mut *boxed as *mut UpdateCallback).cast();
        unsafe {
            (self.api.mpv_render_context_set_update_callback)(
                self.context,
                Some(update_trampoline),
                data,
            )
        };
        self.update_callback = Some(boxed);
    }

    /// Standard OpenGL GL_RGBA8 internal format (0x8058).
    pub const GL_RGBA8: i32 = 0x8058;

    /// Render the current mpv frame into `fbo`. The framebuffer and viewport
    /// must already be bound and set by the caller.
    pub fn render(&self, fbo: i32, width: u32, height: u32) -> Result<()> {
        self.render_with_format(fbo, width, height, Self::GL_RGBA8)
    }

    /// Render the current mpv frame into `fbo` with an explicit texture internal format.
    pub fn render_with_format(
        &self,
        fbo: i32,
        width: u32,
        height: u32,
        internal_format: i32,
    ) -> Result<()> {
        let mut fbo = sys::mpv_opengl_fbo {
            fbo,
            w: width as c_int,
            h: height as c_int,
            internal_format,
        };
        let mut flip_y: c_int = i32::from(self.flip_y);
        let mut params = [
            sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_OPENGL_FBO,
                data: (&mut fbo as *mut sys::mpv_opengl_fbo).cast(),
            },
            sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_FLIP_Y,
                data: (&mut flip_y as *mut c_int).cast(),
            },
            sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let status =
            unsafe { (self.api.mpv_render_context_render)(self.context, params.as_mut_ptr()) };
        if status < 0 {
            return Err(PlayerError::Mpv(unsafe { error_string(&self.api, status) }));
        }
        Ok(())
    }
}

unsafe extern "C" fn update_trampoline(data: *mut c_void) {
    let callback = unsafe { &*(data.cast::<UpdateCallback>()) };
    (callback.0)();
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        unsafe {
            (self.api.mpv_render_context_set_update_callback)(
                self.context,
                None,
                std::ptr::null_mut(),
            );
            (self.api.mpv_render_context_free)(self.context);
        }
        self.update_callback = None;
    }
}

impl sys::MpvApi {
    fn load() -> Result<Self> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        let mut errors = Vec::new();

        if let Ok(executable) = std::env::current_exe()
            && let Some(directory) = executable.parent()
        {
            if cfg!(windows) {
                candidates.push(directory.join("libmpv-2.dll"));
                candidates.push(directory.join("mpv-2.dll"));
            } else {
                candidates.push(directory.join("libmpv.so.2"));
                candidates.push(directory.join("libmpv.so.1"));
                candidates.push(directory.join("libmpv.so"));
                candidates.push(directory.join("../lib/libmpv.so.2"));
                candidates.push(directory.join("../lib/libmpv.so"));
                candidates.push(directory.join("../lib/x86_64-linux-gnu/libmpv.so.2"));
            }
        }

        if cfg!(windows) {
            candidates.push(PathBuf::from("libmpv-2.dll"));
            candidates.push(PathBuf::from("mpv-2.dll"));
        } else {
            for name in ["libmpv.so.2", "libmpv.so.1", "libmpv.so"] {
                candidates.push(PathBuf::from(name));
            }
        }

        for candidate in candidates {
            match unsafe { libloading::Library::new(&candidate) } {
                Ok(library) => match unsafe { Self::from_library(library) } {
                    Ok(api) => return Ok(api),
                    Err(error) => errors.push(error.to_string()),
                },
                Err(error) => {
                    let mut detail = format!("{}: {error}", candidate.display());
                    if cfg!(windows) && candidate.is_file() {
                        detail.push_str(
                            " (the DLL exists but Windows could not load it; check its dependent DLLs and update the graphics driver for the Vulkan loader)",
                        );
                    }
                    errors.push(detail);
                }
            }
        }

        Err(PlayerError::Library(format!(
            "no usable libmpv found ({}). Windows builds require libmpv-2.dll beside the executable; \
             on Linux, install the libmpv package.",
            errors.join("; ")
        )))
    }

    unsafe fn from_library(library: libloading::Library) -> Result<Self> {
        macro_rules! symbol {
            ($name:literal, $type:ty) => {{
                let symbol: libloading::Symbol<$type> = unsafe {
                    library
                        .get(concat!($name, "\0").as_bytes())
                        .map_err(|error| PlayerError::Library(format!("{}: {error}", $name)))?
                };
                *symbol
            }};
        }

        let client_api_version = symbol!("mpv_client_api_version", unsafe extern "C" fn() -> u64);
        let version = unsafe { client_api_version() };
        if version < CLIENT_API_VERSION {
            return Err(PlayerError::Library(format!(
                "libmpv client API {}.{} is too old; {}.{} or newer is required",
                version >> 16,
                version & 0xffff,
                CLIENT_API_VERSION >> 16,
                CLIENT_API_VERSION & 0xffff,
            )));
        }

        Ok(Self {
            mpv_create: symbol!("mpv_create", unsafe extern "C" fn() -> *mut sys::mpv_handle),
            mpv_initialize: symbol!(
                "mpv_initialize",
                unsafe extern "C" fn(*mut sys::mpv_handle) -> c_int
            ),
            mpv_terminate_destroy: symbol!(
                "mpv_terminate_destroy",
                unsafe extern "C" fn(*mut sys::mpv_handle)
            ),
            mpv_set_option_string: symbol!(
                "mpv_set_option_string",
                unsafe extern "C" fn(*mut sys::mpv_handle, *const c_char, *const c_char) -> c_int
            ),
            mpv_set_property_string: symbol!(
                "mpv_set_property_string",
                unsafe extern "C" fn(*mut sys::mpv_handle, *const c_char, *const c_char) -> c_int
            ),
            mpv_get_property_string: symbol!(
                "mpv_get_property_string",
                unsafe extern "C" fn(*mut sys::mpv_handle, *const c_char) -> *mut c_char
            ),
            mpv_get_property: symbol!(
                "mpv_get_property",
                unsafe extern "C" fn(
                    *mut sys::mpv_handle,
                    *const c_char,
                    c_int,
                    *mut c_void,
                ) -> c_int
            ),
            mpv_free_node_contents: symbol!(
                "mpv_free_node_contents",
                unsafe extern "C" fn(*mut sys::mpv_node)
            ),
            mpv_free: symbol!("mpv_free", unsafe extern "C" fn(*mut c_void)),
            mpv_command: symbol!(
                "mpv_command",
                unsafe extern "C" fn(*mut sys::mpv_handle, *const *const c_char) -> c_int
            ),
            mpv_wait_event: symbol!(
                "mpv_wait_event",
                unsafe extern "C" fn(*mut sys::mpv_handle, f64) -> *mut sys::mpv_event
            ),
            mpv_request_log_messages: symbol!(
                "mpv_request_log_messages",
                unsafe extern "C" fn(*mut sys::mpv_handle, *const c_char) -> c_int
            ),
            mpv_error_string: symbol!(
                "mpv_error_string",
                unsafe extern "C" fn(c_int) -> *const c_char
            ),
            mpv_client_api_version: client_api_version,
            mpv_render_context_create: symbol!(
                "mpv_render_context_create",
                unsafe extern "C" fn(
                    *mut *mut sys::mpv_render_context,
                    *mut sys::mpv_handle,
                    *mut sys::mpv_render_param,
                ) -> c_int
            ),
            mpv_render_context_set_update_callback: symbol!(
                "mpv_render_context_set_update_callback",
                unsafe extern "C" fn(
                    *mut sys::mpv_render_context,
                    Option<unsafe extern "C" fn(*mut c_void)>,
                    *mut c_void,
                )
            ),
            mpv_render_context_render: symbol!(
                "mpv_render_context_render",
                unsafe extern "C" fn(
                    *mut sys::mpv_render_context,
                    *mut sys::mpv_render_param,
                ) -> c_int
            ),
            mpv_render_context_free: symbol!(
                "mpv_render_context_free",
                unsafe extern "C" fn(*mut sys::mpv_render_context)
            ),
            _library: library,
        })
    }
}

/// Options for a load request.
#[derive(Debug, Clone, Default)]
pub struct LoadRequest {
    pub url: String,
    pub headers: Vec<String>,
    pub start_position: Option<f64>,
}

impl Player {
    pub fn load_request(&self, request: &LoadRequest) -> Result<()> {
        self.load_with_start(&request.url, &request.headers, request.start_position)
    }
}

pub fn parse_header(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || !value.contains(':') {
        return Err(PlayerError::InvalidArgument(format!(
            "HTTP header must use 'Name: value' format, got {value:?}"
        )));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_header_valid() {
        assert_eq!(
            parse_header("User-Agent: AdultHub/1.0").unwrap(),
            "User-Agent: AdultHub/1.0"
        );
        assert_eq!(
            parse_header("  Authorization: Bearer secret  ").unwrap(),
            "Authorization: Bearer secret"
        );
    }

    #[test]
    fn parse_header_invalid() {
        assert!(parse_header("").is_err());
        assert!(parse_header("   ").is_err());
        assert!(parse_header("InvalidHeaderWithoutColon").is_err());
    }

    #[test]
    fn stats_hud_text_formatting() {
        let empty = PlaybackStats::default();
        let empty_hud = empty.to_hud_text();
        assert!(empty_hud.contains("Resolution: Unknown"));
        assert!(empty_hud.contains("Buffer Cache: N/A"));
        assert!(empty_hud.contains("Container FPS: N/A"));
        assert!(empty_hud.contains("File size: Unavailable"));

        let populated = PlaybackStats {
            width: Some(1920),
            height: Some(1080),
            video_codec: Some("h264".into()),
            audio_codec: Some("aac".into()),
            decoder: Some("h264_vaapi".into()),
            hwdec: Some("vaapi".into()),
            vo: Some("gpu".into()),
            container_fps: Some(60.0),
            video_bitrate: Some(5_000_000.0),
            audio_bitrate: Some(192_000.0),
            dropped_frames: Some(2),
            pixelformat: Some("yuv420p".into()),
            color_primaries: Some("bt709".into()),
            gamma: Some("bt709".into()),
            file_format: Some("mp4".into()),
            cache_duration_secs: Some(14.5),
            cache_forward_bytes: Some(40_000_000),
            cache_total_bytes: Some(60_000_000),
            raw_input_rate_bps: Some(5_000_000.0),
            paused_for_cache: false,
            buffer_profile_name: Some("Standard".into()),
            buffer_profile_details: Some("128M / 25s, wait: 2.5s".into()),
            stall_count: 1,
            total_stalled_duration_secs: 2.1,
            last_stall_diagnosis: Some("TransientBandwidthDrop".into()),
            file_size: Some(104_857_600),
            quality_variant: Some("1080p (Original)".into()),
        };
        let hud = populated.to_hud_text();
        assert!(hud.contains("Resolution: 1920x1080"));
        assert!(hud.contains("Quality Variant: 1080p (Original)"));
        assert!(hud.contains("Video Codec: h264"));
        assert!(hud.contains("Audio Codec: aac"));
        assert!(hud.contains("Decoder: h264_vaapi (HW: vaapi)"));
        assert!(hud.contains("File size: 100.0 MB"));
        assert!(hud.contains("Buffer Profile: Standard (128M / 25s, wait: 2.5s)"));
        assert!(hud.contains("Buffer Cache: 14.5 s"));
        assert!(hud.contains("Container FPS: 60.00"));
        assert!(hud.contains("Video Bitrate: 5.00 Mbit/s (5000 kbit/s)"));
        assert!(hud.contains("Download Rate: 40.00 Mbit/s (4.77 MB/s)"));
        assert!(hud.contains("Stalls: 1 (2.1 s total) - TransientBandwidthDrop"));
        assert!(hud.contains("Dropped Frames: 2"));
        assert!(hud.contains("Pixel format: yuv420p"));
        assert!(hud.contains("Color: bt709 / bt709 (SDR mapped)"));
        assert!(hud.contains("Container: mp4"));
    }

    #[test]
    fn buffer_profile_parameters() {
        let fast = BufferProfile::FastStart;
        assert_eq!(fast.id(), "fast_start");
        assert_eq!(fast.name(), "Fast Start");
        assert_eq!(fast.demuxer_max_bytes(), "48M");
        assert_eq!(fast.demuxer_readahead_secs(), "15");
        assert_eq!(fast.cache_secs(), "15");
        assert_eq!(fast.cache_pause_wait(), "1.5");

        let std = BufferProfile::Standard;
        assert_eq!(std.id(), "standard");
        assert_eq!(std.name(), "Standard");
        assert_eq!(std.demuxer_max_bytes(), "160M");
        assert_eq!(std.demuxer_readahead_secs(), "45");
        assert_eq!(std.cache_secs(), "45");
        assert_eq!(std.cache_pause_wait(), "3.5");

        let large = BufferProfile::Large;
        assert_eq!(large.id(), "large");
        assert_eq!(large.name(), "Large (4K)");
        assert_eq!(large.demuxer_max_bytes(), "384M");
        assert_eq!(large.demuxer_readahead_secs(), "90");
        assert_eq!(large.cache_secs(), "90");
        assert_eq!(large.cache_pause_wait(), "5.5");

        let low = BufferProfile::LowMemory;
        assert_eq!(low.id(), "low_memory");
        assert_eq!(low.name(), "Low-Memory");
        assert_eq!(low.demuxer_max_bytes(), "24M");
        assert_eq!(low.demuxer_readahead_secs(), "8");
        assert_eq!(low.cache_secs(), "8");
        assert_eq!(low.cache_pause_wait(), "1.5");

        assert_eq!(BufferProfile::from_id("fast_start"), Some(BufferProfile::FastStart));
        assert_eq!(BufferProfile::from_id("standard"), Some(BufferProfile::Standard));
        assert_eq!(BufferProfile::from_id("large"), Some(BufferProfile::Large));
        assert_eq!(BufferProfile::from_id("4k"), Some(BufferProfile::Large));
        assert_eq!(BufferProfile::from_id("low_memory"), Some(BufferProfile::LowMemory));
        assert_eq!(BufferProfile::from_id("unknown"), None);
    }

    #[test]
    fn player_error_display() {
        let err = PlayerError::Library("missing libmpv.so".into());
        assert_eq!(err.to_string(), "cannot load libmpv: missing libmpv.so");

        let err = PlayerError::Mpv("invalid property".into());
        assert_eq!(err.to_string(), "libmpv error: invalid property");

        let err = PlayerError::InvalidArgument("bad arg".into());
        assert_eq!(err.to_string(), "invalid player argument: bad arg");
    }

    #[test]
    fn track_model_properties() {
        let track = Track {
            id: 1,
            kind: TrackKind::Audio,
            title: "English".into(),
            language: Some("eng".into()),
            codec: Some("aac".into()),
            selected: true,
        };
        assert_eq!(track.id, 1);
        assert_eq!(track.kind, TrackKind::Audio);
        assert!(track.selected);
    }

    #[test]
    fn test_pcm_audio_output() {
        unsafe {
            std::env::set_var("ADULT_HUB_AO", "pcm");
            std::env::set_var("ADULT_HUB_AO_PCM_FILE", "test_ao.wav");
        }
        let player = Player::new();
        match player {
            Ok(p) => {
                let _ = p.load_with_start("desktop/sample.mp4", &[], Some(1.5));
                std::thread::sleep(std::time::Duration::from_millis(600));
                let _ = p.retry_current_at(2.0);
                std::thread::sleep(std::time::Duration::from_millis(300));
                let _ = std::fs::remove_file("test_ao.wav");
            }
            Err(e) => {
                eprintln!("Player::new failed: {e}");
            }
        }
    }
}
