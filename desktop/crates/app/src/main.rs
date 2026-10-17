#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod renderer;

use adult_hub_player::{PlaybackStats, Player, parse_header};
use slint::{Timer, TimerMode};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

slint::include_modules!();

const ASPECTS: [(Option<&str>, &str); 4] = [
    (None, "Auto"),
    (Some("16:9"), "16:9"),
    (Some("4:3"), "4:3"),
    (Some("2.35:1"), "2.35:1"),
];

#[derive(Default)]
struct Options {
    url: Option<String>,
    headers: Vec<String>,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--header" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--header requires a value".to_owned())?;
                    options
                        .headers
                        .push(parse_header(&value).map_err(|error| error.to_string())?);
                }
                "--" => continue,
                _ if argument.starts_with("--header=") => {
                    let value = argument.trim_start_matches("--header=");
                    options
                        .headers
                        .push(parse_header(value).map_err(|error| error.to_string())?);
                }
                _ if argument.starts_with('-') => {
                    return Err(format!("unknown argument: {argument}"));
                }
                _ => {
                    if options.url.is_some() {
                        return Err("only one media URL can be passed".to_owned());
                    }
                    options.url = Some(argument);
                }
            }
        }
        Ok(options)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    slint::BackendSelector::new().require_opengl().select()?;

    let ui = MainWindow::new()?;
    let options = Options::parse(std::env::args().skip(1))?;

    let player = match Player::new() {
        Ok(player) => Some(Rc::new(player)),
        Err(error) => {
            show_error(&ui, error.to_string());
            None
        }
    };

    if let Some(player) = &player {
        if let Err(error) = player.set_volume(100.0) {
            eprintln!("cannot set initial volume: {error}");
        }
    }

    install_controls(&ui, player.clone(), options.headers.clone());
    install_fullscreen(&ui);

    let frame_pending = Arc::new(AtomicBool::new(false));
    if player.is_some() {
        install_renderer(&ui, player.clone().expect("player present"), &frame_pending)?;
    }

    if let (Some(player), Some(initial)) = (&player, initial_media(&options)) {
        load_media(&ui, player, &initial, &options.headers);
        ui.set_url_text(initial.into());
    } else if player.is_some() {
        ui.set_status("Enter a media URL or path, or use Sample.".into());
    }

    install_polling(&ui, player.clone(), frame_pending);

    ui.show()?;
    ui.run()?;
    Ok(())
}

fn initial_media(options: &Options) -> Option<String> {
    if let Some(url) = &options.url {
        return Some(url.clone());
    }
    media_beside_executable("sample.mp4").or_else(|| {
        let local = PathBuf::from("sample.mp4");
        local.exists().then(|| local.to_string_lossy().into_owned())
    })
}

fn media_beside_executable(name: &str) -> Option<String> {
    let executable = std::env::current_exe().ok()?;
    let candidate = executable.parent()?.join(name);
    candidate
        .is_file()
        .then(|| candidate.to_string_lossy().into_owned())
}

fn install_controls(ui: &MainWindow, player: Option<Rc<Player>>, headers: Vec<String>) {
    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_toggle_play(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else {
                show_error(&ui, "libmpv is not available");
                return;
            };
            match player.toggle_pause() {
                Ok(paused) => ui.set_playing(!paused),
                Err(error) => show_error(&ui, error.to_string()),
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_seek_to(move |seconds| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            if let Err(error) = player.seek_absolute(seconds as f64) {
                show_error(&ui, error.to_string());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_seek_relative(move |seconds| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            if let Err(error) = player.seek_relative(seconds as f64) {
                show_error(&ui, error.to_string());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_set_volume(move |volume| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            if let Err(error) = player.set_volume(volume as f64) {
                show_error(&ui, error.to_string());
            }
            if (volume - ui.get_volume()).abs() > 0.5 {
                ui.set_volume(volume);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let headers = headers.clone();
        ui.on_load_url(move |text| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else {
                show_error(&ui, "libmpv is not available");
                return;
            };
            let text = text.to_string();
            if text.trim().is_empty() {
                show_error(&ui, "Enter a media URL or file path first");
                return;
            }
            ui.set_url_text(text.clone().into());
            load_media(&ui, player, &text, &headers);
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let headers = headers.clone();
        ui.on_load_sample(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            match media_beside_executable("sample.mp4") {
                Some(path) => {
                    ui.set_url_text(path.clone().into());
                    load_media(&ui, player, &path, &headers);
                }
                None => show_error(&ui, "sample.mp4 was not found next to the executable"),
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let headers = headers.clone();
        ui.on_open_file(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            match pick_media_file() {
                Some(path) => {
                    ui.set_url_text(path.clone().into());
                    load_media(&ui, player, &path, &headers);
                }
                None => {
                    if !cfg!(windows) {
                        ui.set_status(
                            "Native file dialog is only enabled in the Windows prototype; \
                             enter a path or URL instead."
                                .into(),
                        );
                        ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
                    }
                }
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let index = Rc::new(Cell::new(0usize));
        ui.on_cycle_aspect(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let next = (index.get() + 1) % ASPECTS.len();
            index.set(next);
            let (aspect, label) = ASPECTS[next];
            if let Err(error) = player.set_aspect_override(aspect) {
                show_error(&ui, error.to_string());
                return;
            }
            ui.set_aspect_label(format!("Aspect: {label}").into());
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_toggle_hud(move || {
            let Some(ui) = weak.upgrade() else { return };
            let visible = !ui.get_hud_visible();
            ui.set_hud_visible(visible);
            if !visible {
                ui.set_hud_text("".into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_toggle_mute(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let muted = !player.is_muted();
            if let Err(error) = player.set_muted(muted) {
                show_error(&ui, error.to_string());
                return;
            }
            ui.set_muted(muted);
        });
    }
}

fn install_fullscreen(ui: &MainWindow) {
    let fullscreen = Rc::new(Cell::new(false));

    {
        let ui_weak = ui.as_weak();
        let fullscreen = fullscreen.clone();
        ui.on_toggle_fullscreen(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let next = !fullscreen.get();
            ui.window().set_fullscreen(next);
            fullscreen.set(next);
        });
    }

    {
        let ui_weak = ui.as_weak();
        let fullscreen = fullscreen.clone();
        ui.on_escape_pressed(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            if fullscreen.get() {
                ui.window().set_fullscreen(false);
                fullscreen.set(false);
            }
        });
    }
}

fn install_renderer(
    ui: &MainWindow,
    player: Rc<Player>,
    frame_pending: &Arc<AtomicBool>,
) -> Result<(), slint::SetRenderingNotifierError> {
    let ui_weak = ui.as_weak();
    let frame_pending = frame_pending.clone();
    let flip_y = std::env::var("ADULT_HUB_FLIP_Y")
        .map(|value| value != "0")
        .unwrap_or(true);
    let mut renderer: Option<renderer::Renderer> = None;

    ui.window()
        .set_rendering_notifier(move |state, graphics_api| match state {
            slint::RenderingState::RenderingSetup => {
                let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else {
                    if let Some(ui) = ui_weak.upgrade() {
                        show_error(&ui, "the selected Slint renderer is not OpenGL");
                    }
                    return;
                };
                let get_proc_address: &dyn Fn(&std::ffi::CStr) -> *const std::ffi::c_void =
                    *get_proc_address;
                match renderer::Renderer::new(Rc::clone(&player), get_proc_address, flip_y) {
                    Ok(mut created) => {
                        let pending = frame_pending.clone();
                        created.set_update_callback(move || {
                            pending.store(true, Ordering::Release);
                        });
                        renderer = Some(created);
                    }
                    Err(error) => {
                        if let Some(ui) = ui_weak.upgrade() {
                            show_error(&ui, error);
                        }
                    }
                }
            }
            slint::RenderingState::BeforeRendering => {
                let Some(renderer) = renderer.as_mut() else {
                    return;
                };
                let Some(ui) = ui_weak.upgrade() else { return };
                let width = ui.get_texture_width().max(2) as u32;
                let height = ui.get_texture_height().max(2) as u32;
                match renderer.render(width, height) {
                    Ok(Some(image)) => ui.set_video_image(image),
                    Ok(None) => {}
                    Err(error) => show_error(&ui, error),
                }
            }
            slint::RenderingState::RenderingTeardown => {
                renderer = None;
            }
            _ => {}
        })
}

fn install_polling(ui: &MainWindow, player: Option<Rc<Player>>, frame_pending: Arc<AtomicBool>) {
    let ui_weak = ui.as_weak();
    let timer = Timer::default();
    let mut tick: u64 = 0;

    timer.start(TimerMode::Repeated, Duration::from_millis(33), move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        if frame_pending.swap(false, Ordering::AcqRel) {
            ui.window().request_redraw();
        }
        let Some(player) = &player else { return };

        for event in player.events().try_iter() {
            handle_event(&ui, player, event);
        }

        let idle = player.get_string("idle-active").as_deref() == Some("yes");
        let paused = player.is_paused() || idle;
        if paused == ui.get_playing() {
            ui.set_playing(!paused);
        }

        if let Some(position) = player.position()
            && (position - ui.get_position() as f64).abs() > 0.02
        {
            ui.set_position(position as f32);
            ui.set_position_text(format_time(position).into());
        }

        if let Some(duration) = player.duration()
            && (duration - ui.get_duration() as f64).abs() > 0.25
        {
            ui.set_duration(duration as f32);
            ui.set_duration_text(format_time(duration).into());
        }

        if let Some(volume) = player.volume()
            && (volume - ui.get_volume() as f64).abs() > 0.5
        {
            ui.set_volume(volume as f32);
        }

        tick = tick.wrapping_add(1);
        if ui.get_hud_visible() && tick % 15 == 0 {
            ui.set_hud_text(stats_text(&player.stats()).into());
        }
    });
}

fn handle_event(ui: &MainWindow, player: &Player, event: adult_hub_player::PlayerEvent) {
    use adult_hub_player::PlayerEvent;
    match event {
        PlayerEvent::FileLoaded { width, height } => {
            update_texture_size(ui, width, height);
            let name = player
                .get_string("filename")
                .and_then(|path| {
                    Path::new(&path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "media".to_owned());
            ui.set_status(format!("Loaded: {name}").into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            ui.set_playing(!player.is_paused());
        }
        PlayerEvent::VideoReconfig { width, height } => {
            update_texture_size(ui, width, height);
        }
        PlayerEvent::EndFile { eof, error } => {
            ui.set_playing(false);
            if let Some(error) = error {
                show_error(ui, format!("Playback error: {error}"));
            } else if eof {
                ui.set_status("Playback ended.".into());
                ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
            }
        }
        PlayerEvent::Shutdown => {}
    }
}

fn update_texture_size(ui: &MainWindow, width: Option<u32>, height: Option<u32>) {
    if let (Some(width), Some(height)) = (width, height)
        && width > 0
        && height > 0
    {
        ui.set_texture_width(width as i32);
        ui.set_texture_height(height as i32);
    }
}

fn stats_text(stats: &PlaybackStats) -> String {
    let text = stats.to_hud_text();
    if text.is_empty() {
        "No playback statistics yet.".to_owned()
    } else {
        text
    }
}

fn show_error(ui: &MainWindow, message: impl Into<slint::SharedString>) {
    ui.set_status(message.into());
    ui.set_status_color(slint::Color::from_rgb_u8(255, 118, 118));
}

fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "0:00".to_owned();
    }
    let total = seconds.round() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn load_media(ui: &MainWindow, player: &Player, input: &str, headers: &[String]) {
    let path = resolve_media_input(input);
    ui.set_status(format!("Loading: {path}").into());
    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
    if let Err(error) = player.load_with_start(&path, headers, None) {
        show_error(ui, error.to_string());
    }
}

fn resolve_media_input(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("rtsp://")
        || trimmed.starts_with("rtmp://")
        || trimmed.starts_with("file://")
    {
        return trimmed.to_owned();
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return trimmed.to_owned();
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let candidate = directory.join(path);
        if candidate.exists() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    trimmed.to_owned()
}

#[cfg(windows)]
fn pick_media_file() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Open media")
        .add_filter(
            "Video",
            &[
                "mp4", "mkv", "webm", "mov", "avi", "ts", "m4v", "mpg", "mpeg",
            ],
        )
        .pick_file()
        .map(|path| path.to_string_lossy().into_owned())
}

#[cfg(not(windows))]
fn pick_media_file() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_parse_empty() {
        let opts = Options::parse(Vec::<String>::new()).unwrap();
        assert!(opts.url.is_none());
        assert!(opts.headers.is_empty());
    }

    #[test]
    fn options_parse_url() {
        let opts = Options::parse(["http://example.com/video.mp4".to_string()]).unwrap();
        assert_eq!(opts.url.as_deref(), Some("http://example.com/video.mp4"));
        assert!(opts.headers.is_empty());
    }

    #[test]
    fn options_parse_headers() {
        let opts = Options::parse([
            "--header".to_string(),
            "Authorization: Bearer secret".to_string(),
            "--header=User-Agent: Desktop/1.0".to_string(),
            "sample.mp4".to_string(),
        ])
        .unwrap();
        assert_eq!(opts.url.as_deref(), Some("sample.mp4"));
        assert_eq!(
            opts.headers,
            vec![
                "Authorization: Bearer secret".to_string(),
                "User-Agent: Desktop/1.0".to_string(),
            ]
        );
    }

    #[test]
    fn options_parse_errors() {
        assert!(Options::parse(["--header".to_string()]).is_err());
        assert!(Options::parse(["--unknown-flag".to_string()]).is_err());
        assert!(Options::parse(["url1".to_string(), "url2".to_string()]).is_err());
    }

    #[test]
    fn format_time_various_durations() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(-10.0), "0:00");
        assert_eq!(format_time(f64::NAN), "0:00");
        assert_eq!(format_time(f64::INFINITY), "0:00");
        assert_eq!(format_time(9.0), "0:09");
        assert_eq!(format_time(65.0), "1:05");
        assert_eq!(format_time(599.0), "9:59");
        assert_eq!(format_time(3600.0), "1:00:00");
        assert_eq!(format_time(3665.0), "1:01:05");
    }

    #[test]
    fn resolve_media_input_urls() {
        assert_eq!(
            resolve_media_input("https://server.local/media/1.mp4"),
            "https://server.local/media/1.mp4"
        );
        assert_eq!(
            resolve_media_input("http://server.local/stream.m3u8"),
            "http://server.local/stream.m3u8"
        );
        assert_eq!(
            resolve_media_input("file:///var/media/sample.mp4"),
            "file:///var/media/sample.mp4"
        );
        assert_eq!(
            resolve_media_input("/opt/media/video.mp4"),
            "/opt/media/video.mp4"
        );
    }
}
