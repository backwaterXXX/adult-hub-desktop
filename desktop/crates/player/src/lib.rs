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
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;

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
    pub fn name(&self) -> &'static str {
        match self {
            Self::FastStart => "Fast Start",
            Self::Standard => "Standard",
            Self::Large => "Large",
            Self::LowMemory => "Low-Memory",
        }
    }

    pub fn demuxer_max_bytes(&self) -> &'static str {
        match self {
            Self::FastStart => "16M",
            Self::Standard => "64M",
            Self::Large => "150M",
            Self::LowMemory => "8M",
        }
    }

    pub fn demuxer_max_back_bytes(&self) -> &'static str {
        match self {
            Self::FastStart => "8M",
            Self::Standard => "32M",
            Self::Large => "64M",
            Self::LowMemory => "2M",
        }
    }

    pub fn demuxer_readahead_secs(&self) -> &'static str {
        match self {
            Self::FastStart => "5",
            Self::Standard => "20",
            Self::Large => "60",
            Self::LowMemory => "3",
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
    pub file_format: Option<String>,
    pub color_primaries: Option<String>,
    pub gamma: Option<String>,
    pub pixelformat: Option<String>,
    pub vo: Option<String>,
    pub cache_duration_secs: Option<f64>,
    pub paused_for_cache: bool,
}

impl PlaybackStats {
    pub fn to_hud_text(&self) -> String {
        let mut lines = Vec::new();
        let res = match (self.width, self.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => format!("{w}x{h}"),
            _ => "Unknown".to_string(),
        };
        lines.push(format!("Resolution: {res}"));

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

        if let Some(cache_secs) = self.cache_duration_secs {
            let state = if self.paused_for_cache { " (Buffering...)" } else { "" };
            lines.push(format!("Buffer Cache: {cache_secs:.1} s{state}"));
        } else {
            lines.push("Buffer Cache: N/A".to_string());
        }

        if let Some(fps) = self.container_fps {
            lines.push(format!("Container FPS: {fps:.2}"));
        } else {
            lines.push("Container FPS: N/A".to_string());
        }

        if let Some(bitrate) = self.video_bitrate {
            lines.push(format!("Video Bitrate: {:.0} kbit/s", bitrate / 1000.0));
        } else {
            lines.push("Video Bitrate: N/A".to_string());
        }

        let dropped = self.dropped_frames.unwrap_or(0);
        lines.push(format!("Dropped Frames: {dropped}"));

        if let Some(pixelformat) = &self.pixelformat {
            lines.push(format!("Pixel format: {pixelformat}"));
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

pub struct Player {
    api: Arc<sys::MpvApi>,
    handle: *mut sys::mpv_handle,
    events: Receiver<PlayerEvent>,
    event_thread: Option<JoinHandle<()>>,
    quit: Arc<AtomicBool>,
}

unsafe impl Send for Player {}
unsafe impl Sync for Player {}

/// The raw libmpv handle is safe to use from the event thread: libmpv
/// documents that calls on the same handle from different threads are safe.
struct SendHandle(*mut sys::mpv_handle);
unsafe impl Send for SendHandle {}

impl Player {
    pub fn new() -> Result<Self> {
        let api = Arc::new(sys::MpvApi::load()?);
        let handle = unsafe { (api.mpv_create)() };
        if handle.is_null() {
            return Err(PlayerError::Mpv("mpv_create returned null".into()));
        }

        // Isolated playback: never inherit the user's mpv configuration,
        // scripts or key bindings.
        for (name, value) in [
            ("config", "no"),
            ("load-scripts", "no"),
            ("input-default-bindings", "no"),
            ("osc", "no"),
            ("terminal", "no"),
            ("idle", "yes"),
            ("vo", "libmpv"),
            ("gpu-api", "opengl"),
            ("hwdec", "auto-safe"),
            ("keep-open", "no"),
            ("audio-client-name", "Adult Hub Desktop"),
            ("title", "Adult Hub Desktop"),
            ("msg-level", "all=warn"),
            ("tone-mapping", "auto"),
            ("target-colorspace-hint", "no"),
            ("target-trc", "srgb"),
            ("cache", "yes"),
            ("demuxer-max-bytes", "64M"),
            ("demuxer-max-back-bytes", "32M"),
            ("demuxer-readahead-secs", "20"),
            ("vd-lavc-threads", "0"),
            ("hr-seek", "yes"),
            ("video-sync", "audio"),
        ] {
            if let Err(error) = unsafe { set_option_raw(&api, handle, name, value) } {
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
        let header_value = headers.join(",");
        self.set_property_string("http-header-fields", &header_value)?;
        match start_position {
            Some(seconds) if seconds > 0.0 => {
                self.command(&["loadfile", url, "replace", &format!("start={seconds:.3}")])
            }
            _ => self.command(&["loadfile", url, "replace"]),
        }
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
        self.set_property_string("time-pos", &format!("{seconds:.3}"))
    }

    pub fn seek_relative(&self, seconds: f64) -> Result<()> {
        self.command(&["seek", &format!("{seconds:.3}"), "relative"])
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
        self.set_property_string("demuxer-max-bytes", profile.demuxer_max_bytes())?;
        self.set_property_string("demuxer-max-back-bytes", profile.demuxer_max_back_bytes())?;
        self.set_property_string("demuxer-readahead-secs", profile.demuxer_readahead_secs())?;
        Ok(())
    }

    pub fn set_hwdec(&self, hwdec: &str) -> Result<()> {
        self.set_property_string("hwdec", hwdec)
    }

    pub fn current_hwdec(&self) -> Option<String> {
        self.get_string("hwdec-current")
            .or_else(|| self.get_string("hwdec"))
    }

    pub fn stats(&self) -> PlaybackStats {
        let width = self.get_u32("width");
        let height = self.get_u32("height");
        let cache_duration_secs = self.get_f64("demuxer-cache-duration");
        let paused_for_cache = self.get_string("paused-for-cache").as_deref() == Some("yes");

        PlaybackStats {
            video_codec: self.get_string("video-codec"),
            audio_codec: self.get_string("audio-codec-name"),
            width,
            height,
            decoder: self.get_string("video-params/pixelformat"),
            hwdec: self.get_string("hwdec-current"),
            dropped_frames: self
                .get_string("decoder-frame-drop-count")
                .and_then(|value| value.parse().ok()),
            container_fps: self.get_f64("container-fps"),
            video_bitrate: self.get_f64("video-bitrate"),
            file_format: self.get_string("file-format"),
            color_primaries: self.get_string("video-params/primaries"),
            gamma: self.get_string("video-params/gamma"),
            pixelformat: self.get_string("video-params/pixelformat"),
            vo: self.get_string("current-vo"),
            cache_duration_secs,
            paused_for_cache,
        }
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
        if value.is_empty() || value == "no" {
            None
        } else {
            Some(value)
        }
    }

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        self.get_string(name)?.parse().ok()
    }

    pub fn get_u32(&self, name: &str) -> Option<u32> {
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
            sys::MPV_EVENT_SHUTDOWN => {
                let _ = sender.send(PlayerEvent::Shutdown);
                break;
            }
            sys::MPV_EVENT_FILE_LOADED => {
                let _ = sender.send(PlayerEvent::FileLoaded {
                    width: get_u32_handle(&api, handle, "width"),
                    height: get_u32_handle(&api, handle, "height"),
                });
            }
            sys::MPV_EVENT_VIDEO_RECONFIG | sys::MPV_EVENT_PLAYBACK_RESTART => {
                let _ = sender.send(PlayerEvent::VideoReconfig {
                    width: get_u32_handle(&api, handle, "width"),
                    height: get_u32_handle(&api, handle, "height"),
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
    _player: Rc<Player>,
}

struct UpdateCallback(Box<dyn Fn() + Send>);

pub type ProcAddressFn<'a> = dyn Fn(&CStr) -> *const c_void + 'a;

impl RenderContext {
    /// Must be called during `RenderingSetup` with the OpenGL context current.
    pub fn new(
        player: &Rc<Player>,
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
            _player: Rc::clone(player),
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

    /// Render the current mpv frame into `fbo`. The framebuffer and viewport
    /// must already be bound and set by the caller.
    pub fn render(&self, fbo: i32, width: u32, height: u32) -> Result<()> {
        let mut fbo = sys::mpv_opengl_fbo {
            fbo,
            w: width as c_int,
            h: height as c_int,
            internal_format: 0,
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
        if cfg!(windows) {
            candidates.push(PathBuf::from("libmpv-2.dll"));
            candidates.push(PathBuf::from("mpv-2.dll"));
            if let Ok(executable) = std::env::current_exe()
                && let Some(directory) = executable.parent()
            {
                candidates.push(directory.join("libmpv-2.dll"));
            }
        } else {
            for name in ["libmpv.so.2", "libmpv.so.1", "libmpv.so"] {
                candidates.push(PathBuf::from(name));
            }
        }

        let mut errors = Vec::new();
        for candidate in candidates {
            match unsafe { libloading::Library::new(&candidate) } {
                Ok(library) => match unsafe { Self::from_library(library) } {
                    Ok(api) => return Ok(api),
                    Err(error) => errors.push(error.to_string()),
                },
                Err(error) => errors.push(format!("{}: {error}", candidate.display())),
            }
        }

        Err(PlayerError::Library(format!(
            "no usable libmpv found ({}). Place libmpv-2.dll next to the executable on Windows, \
             or install the libmpv package on Linux.",
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
                "libmpv client API {}.{} is too old; 2.6 or newer is required",
                version >> 16,
                version & 0xffff
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
            mpv_free: symbol!("mpv_free", unsafe extern "C" fn(*mut c_void)),
            mpv_command: symbol!(
                "mpv_command",
                unsafe extern "C" fn(*mut sys::mpv_handle, *const *const c_char) -> c_int
            ),
            mpv_wait_event: symbol!(
                "mpv_wait_event",
                unsafe extern "C" fn(*mut sys::mpv_handle, f64) -> *mut sys::mpv_event
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
            dropped_frames: Some(2),
            pixelformat: Some("yuv420p".into()),
            color_primaries: Some("bt709".into()),
            gamma: Some("bt709".into()),
            file_format: Some("mp4".into()),
            cache_duration_secs: Some(14.5),
            paused_for_cache: false,
        };
        let hud = populated.to_hud_text();
        assert!(hud.contains("Resolution: 1920x1080"));
        assert!(hud.contains("Video Codec: h264"));
        assert!(hud.contains("Audio Codec: aac"));
        assert!(hud.contains("Decoder: h264_vaapi (HW: vaapi)"));
        assert!(hud.contains("Buffer Cache: 14.5 s"));
        assert!(hud.contains("Container FPS: 60.00"));
        assert!(hud.contains("Video Bitrate: 5000 kbit/s"));
        assert!(hud.contains("Dropped Frames: 2"));
        assert!(hud.contains("Pixel format: yuv420p"));
        assert!(hud.contains("Color: bt709 / bt709 (SDR mapped)"));
        assert!(hud.contains("Container: mp4"));
    }

    #[test]
    fn buffer_profile_parameters() {
        let fast = BufferProfile::FastStart;
        assert_eq!(fast.name(), "Fast Start");
        assert_eq!(fast.demuxer_max_bytes(), "16M");
        assert_eq!(fast.demuxer_readahead_secs(), "5");

        let std = BufferProfile::Standard;
        assert_eq!(std.name(), "Standard");
        assert_eq!(std.demuxer_max_bytes(), "64M");
        assert_eq!(std.demuxer_readahead_secs(), "20");

        let large = BufferProfile::Large;
        assert_eq!(large.name(), "Large");
        assert_eq!(large.demuxer_max_bytes(), "150M");
        assert_eq!(large.demuxer_readahead_secs(), "60");

        let low = BufferProfile::LowMemory;
        assert_eq!(low.name(), "Low-Memory");
        assert_eq!(low.demuxer_max_bytes(), "8M");
        assert_eq!(low.demuxer_readahead_secs(), "3");
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
}
