use crate::MainWindow;
use crate::context::AppContext;
use crate::phash::PhashCoordinator;
use crate::reporter::PlaybackReporter;
use crate::session::PlaybackSessionController;
use adult_hub_client::{JellyfinClient, TvItem};
use adult_hub_player::{PlaybackStats, Player};
use adult_hub_storage::{CredentialStore, LocalStore};
use std::path::Path;
#[cfg(not(windows))]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub fn format_time(seconds: f64) -> String {
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

pub fn resolve_media_input(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed == "sample.mp4" || trimmed == "sample" {
        return crate::options::resolve_sample_media();
    }
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

pub fn show_error(ui: &MainWindow, message: impl Into<slint::SharedString>) {
    let msg = message.into();
    crate::diagnostics::GLOBAL_LOG.e("UI", msg.as_str());
    if ui.get_active_screen() == "player" {
        ui.set_player_media_loaded(false);
        ui.set_player_controls_visible(true);
        ui.set_player_more_controls_visible(false);
        ui.set_player_error_message(msg.clone());
    } else {
        // Startup errors (for example, a missing system graphics runtime)
        // must be visible even when the player screen is not open.
        ui.set_toast_message(msg.clone());
        ui.set_toast_type("error".into());
        ui.set_toast_action_text("".into());
        ui.set_toast_visible(true);
    }
    ui.set_status(msg);
    ui.set_status_color(slint::Color::from_rgb_u8(255, 118, 118));
}

fn reset_playback_speed(ui: &MainWindow, player: &Player) {
    if let Err(error) = player.set_speed(1.0) {
        crate::diagnostics::GLOBAL_LOG.w(
            "Player",
            &format!("could not reset playback speed: {error}"),
        );
    }
    ui.set_speed_label("1.0x".into());
}

pub fn stats_text(stats: &PlaybackStats) -> String {
    let text = stats.to_hud_text();
    if text.is_empty() {
        "No playback statistics yet.".to_owned()
    } else {
        text
    }
}

pub fn update_texture_size(ui: &MainWindow, width: Option<u32>, height: Option<u32>) {
    if let (Some(width), Some(height)) = (width, height)
        && width > 0
        && height > 0
    {
        ui.set_texture_width(width as i32);
        ui.set_texture_height(height as i32);
    }
}

pub fn load_media(ui: &MainWindow, player: &Player, input: &str, headers: &[String]) {
    let path = resolve_media_input(input);
    reset_playback_speed(ui, player);
    ui.set_player_controls_visible(true);
    ui.set_player_more_controls_visible(false);
    ui.set_player_media_loaded(false);
    ui.set_player_title(
        Path::new(&path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Media".to_owned())
            .into(),
    );
    ui.set_player_error_message(String::new().into());
    ui.set_status(format!("Loading: {path}").into());
    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
    if let Err(error) = player.load_with_start(&path, headers, None) {
        show_error(ui, error.to_string());
    }
}

/// Save the current item's final position and release playback when leaving the player screen.
pub fn stop_active_playback_for_navigation(
    ui: &MainWindow,
    player: Option<&Player>,
    reporter: &PlaybackReporter,
    session_ctrl: &PlaybackSessionController,
    current_item: &Arc<Mutex<Option<TvItem>>>,
) {
    let position = player
        .and_then(Player::position)
        .unwrap_or_else(|| ui.get_position() as f64)
        .max(0.0);
    let duration = player
        .and_then(Player::duration)
        .unwrap_or_else(|| ui.get_duration() as f64)
        .max(0.0);
    let position_ms = (position * 1000.0) as u64;
    let duration_ms = (duration * 1000.0) as u64;

    if let Some(item) = current_item.lock().unwrap().take() {
        session_ctrl.checkpoint_progress(
            &item,
            position_ms,
            duration_ms,
            ui.get_playback_ended_visible(),
        );
    }
    reporter.stop_session(position_ms);
    if let Some(player) = player {
        let _ = player.stop();
    }

    ui.set_playing(false);
    ui.set_player_controls_visible(true);
    ui.set_player_more_controls_visible(false);
    ui.set_player_media_loaded(false);
    ui.set_player_title("".into());
    ui.set_position(0.0);
    ui.set_duration(0.0);
    ui.set_position_text("0:00".into());
    ui.set_duration_text("0:00".into());
    ui.set_resume_prompt_visible(false);
    ui.set_playback_ended_visible(false);
    ui.set_autoplay_countdown_sec(0);
    ui.set_player_error_message("".into());
    ui.set_hud_visible(false);
    ui.set_hud_text("".into());
    ui.set_status("Ready".into());
    ui.set_status_color(slint::Color::from_rgb_u8(158, 158, 158));
}

pub fn start_item_playback(
    ui: &MainWindow,
    player: &Player,
    item: TvItem,
    headers: &[String],
    credential_store: &Arc<CredentialStore>,
    local_store: &Arc<LocalStore>,
    jellyfin: &Arc<JellyfinClient>,
    reporter: &Arc<PlaybackReporter>,
    session_ctrl: &Arc<PlaybackSessionController>,
    current_item: &Arc<Mutex<Option<TvItem>>>,
    phash_coord: &Arc<PhashCoordinator>,
) {
    start_item_playback_with_resume(
        ui,
        player,
        item,
        headers,
        credential_store,
        local_store,
        jellyfin,
        reporter,
        session_ctrl,
        current_item,
        phash_coord,
        None,
    );
}

pub fn start_item_playback_with_resume(
    ui: &MainWindow,
    player: &Player,
    item: TvItem,
    headers: &[String],
    credential_store: &Arc<CredentialStore>,
    local_store: &Arc<LocalStore>,
    jellyfin: &Arc<JellyfinClient>,
    reporter: &Arc<PlaybackReporter>,
    session_ctrl: &Arc<PlaybackSessionController>,
    current_item: &Arc<Mutex<Option<TvItem>>>,
    phash_coord: &Arc<PhashCoordinator>,
    resume_choice: Option<bool>,
) {
    session_ctrl.next_request_id();

    // 1. Persist outgoing video's progress and stop session
    if let Some(prev) = current_item.lock().unwrap().take() {
        let pos = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
        let dur = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
        session_ctrl.checkpoint_progress(&prev, pos, dur, false);
        reporter.stop_session(pos);
    }

    // 2. Resolve stream URL
    let session = credential_store.get().ok().flatten();
    let resolver_url = local_store
        .get_setting("resolver_base_url")
        .ok()
        .flatten()
        .or_else(|| local_store.get_setting("resolver_url").ok().flatten())
        .or_else(|| session.as_ref().map(|s| s.server_url.clone()));

    let mut resolved_media_source_id = item.media_source_id.clone();
    let mut play_session_id = None;
    let (stream_url, play_method, required_headers) =
        if item.is_telegram() || item.id == "sample" || uuid::Uuid::parse_str(&item.id).is_err() {
            match PlaybackSessionController::resolve_stream_url(
                &item,
                session.as_ref().map(|s| s.server_url.as_str()),
                resolver_url.as_deref(),
                session.as_ref().map(|s| s.access_token.as_str()),
            ) {
                Ok((url, method)) => (url, method, Vec::new()),
                Err(err) => {
                    if item.is_telegram()
                        && (resolver_url.is_none()
                            || resolver_url.as_deref().unwrap_or("").trim().is_empty())
                    {
                        ui.set_resolver_prompt_dialog_visible(true);
                        ui.set_toast_message(
                        "Resolver URL required to stream Telegram videos. Click here to configure."
                            .into(),
                    );
                        ui.set_toast_type("warning".into());
                        ui.set_toast_action_text("Configure".into());
                        ui.set_toast_visible(true);
                    } else {
                        show_error(ui, err.clone());
                        ui.set_toast_message(err.into());
                        ui.set_toast_type("error".into());
                        ui.set_toast_action_text("".into());
                        ui.set_toast_visible(true);
                    }
                    return;
                }
            }
        } else {
            let Some(session) = session.as_ref() else {
                show_error(ui, "Sign in to Jellyfin before playing this item.");
                return;
            };
            match jellyfin.resolve_playback(
                &session.server_url,
                &session.user_id,
                &session.access_token,
                &item.id,
            ) {
                Ok(resolution) => {
                    resolved_media_source_id = resolution.media_source_id;
                    play_session_id = resolution.play_session_id;
                    (
                        resolution.stream_url,
                        resolution.play_method,
                        resolution.required_headers,
                    )
                }
                Err(err) => {
                    show_error(ui, format!("Could not resolve Jellyfin playback: {err}"));
                    return;
                }
            }
        };

    let mut stream_headers = headers.to_vec();
    stream_headers.extend(required_headers);
    if uuid::Uuid::parse_str(&item.id).is_ok() {
        if let Some(sess) = &session {
            if !sess.access_token.is_empty() {
                stream_headers.push(format!("X-Emby-Token: {}", sess.access_token));
            }
        }
    }

    // 3. Determine resume position
    let duration_ms = item.runtime_ticks.map(|t| (t / 10_000) as u64).unwrap_or(0);
    let mut start_pos_secs = 0.0;
    ui.set_resume_prompt_visible(false);

    let calculated_resume_ms = if item.is_telegram() {
        let prov_id = item
            .provider_video_id
            .as_deref()
            .or_else(|| item.id.strip_prefix("telegram:"))
            .unwrap_or(&item.id);
        session_ctrl.get_telegram_resume_position(prov_id)
    } else if item.playback_position_ticks > 0 {
        Some((item.playback_position_ticks / 10_000) as u64)
    } else {
        local_store
            .get_progress("local", &item.id)
            .ok()
            .flatten()
            .and_then(|(pos, _)| if pos > 0 { Some(pos) } else { None })
    };

    match resume_choice {
        Some(true) => {
            if let Some(resume_ms) = calculated_resume_ms {
                start_pos_secs = resume_ms as f64 / 1000.0;
            }
        }
        Some(false) => {
            start_pos_secs = 0.0;
        }
        None => {
            if item.is_telegram() {
                if let Some(resume_ms) = calculated_resume_ms {
                    if PlaybackSessionController::should_prompt_resume(resume_ms, duration_ms) {
                        ui.set_resume_prompt_position((resume_ms as f32) / 1000.0);
                        ui.set_resume_prompt_time(format_time(resume_ms as f64 / 1000.0).into());
                        ui.set_resume_prompt_visible(true);
                    }
                }
            } else if let Some(resume_ms) = calculated_resume_ms {
                if PlaybackSessionController::should_prompt_resume(resume_ms, duration_ms) {
                    start_pos_secs = resume_ms as f64 / 1000.0;
                }
            }
        }
    }

    // 4. Update UI & current item
    *current_item.lock().unwrap() = Some(item.clone());
    ui.set_player_is_favorite(item.is_favorite);
    ui.set_player_favorite_enabled(!item.is_telegram() && uuid::Uuid::parse_str(&item.id).is_ok());
    ui.set_player_error_message(String::new().into());
    ui.set_player_controls_visible(true);
    ui.set_player_more_controls_visible(false);
    ui.set_player_media_loaded(false);
    ui.set_player_title(crate::catalog::display_title(&item).into());
    ui.set_url_text(stream_url.clone().into());
    ui.set_status(format!("Loading: {}", item.title).into());
    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
    ui.set_active_screen("player".into());

    // 5. Start player & session reporting
    let start_pos_opt = if start_pos_secs > 0.0 {
        Some(start_pos_secs)
    } else {
        None
    };
    let path = resolve_media_input(&stream_url);
    reset_playback_speed(ui, player);
    if let Err(e) = player.load_with_start(&path, &stream_headers, start_pos_opt) {
        show_error(ui, e.to_string());
        return;
    }

    if !item.is_direct_media() {
        match serde_json::to_string(&item) {
            Ok(item_json) => {
                if let Err(error) = local_store.record_watch_history(&item.id, &item_json) {
                    crate::diagnostics::GLOBAL_LOG.w(
                        "WatchHistory",
                        &format!("Could not save watch history: {error}"),
                    );
                }
            }
            Err(error) => crate::diagnostics::GLOBAL_LOG.w(
                "WatchHistory",
                &format!("Could not serialize watch history item: {error}"),
            ),
        }
    }

    let is_jellyfin_item = uuid::Uuid::parse_str(&item.id).is_ok();
    if is_jellyfin_item {
        reporter.start_session(
            &item.id,
            resolved_media_source_id,
            play_session_id,
            play_method,
            (start_pos_secs * 1000.0) as u64,
        );
    }

    // 6. Enqueue pHash video matching if enabled
    let phash_enabled = local_store
        .get_setting("phash_on_playback")
        .ok()
        .flatten()
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    if phash_enabled && (is_jellyfin_item || item.is_telegram()) {
        phash_coord.enqueue_generation(&item, None);
    }
}

#[allow(dead_code)]
pub fn start_item_playback_ctx(ui: &MainWindow, ctx: &AppContext, item: TvItem) {
    start_item_playback_with_resume_ctx(ui, ctx, item, None);
}

#[allow(dead_code)]
pub fn start_item_playback_with_resume_ctx(
    ui: &MainWindow,
    ctx: &AppContext,
    item: TvItem,
    resume_choice: Option<bool>,
) {
    let Some(player) = &ctx.player else {
        show_error(ui, "libmpv is not available");
        return;
    };
    start_item_playback_with_resume(
        ui,
        player,
        item,
        &ctx.headers,
        &ctx.credential_store,
        &ctx.local_store,
        &ctx.jellyfin,
        &ctx.reporter,
        &ctx.session_ctrl,
        &ctx.current_playing_item,
        &ctx.phash_coord,
        resume_choice,
    );
}

pub fn play_random_catalog_item(
    ui: &MainWindow,
    player: &Player,
    headers: &[String],
    credential_store: &Arc<CredentialStore>,
    local_store: &Arc<LocalStore>,
    jellyfin: &Arc<JellyfinClient>,
    reporter: &Arc<PlaybackReporter>,
    session_ctrl: &Arc<PlaybackSessionController>,
    current_item: &Arc<Mutex<Option<TvItem>>>,
    cached_catalog: &Arc<Mutex<Vec<TvItem>>>,
    phash_coord: &Arc<PhashCoordinator>,
) {
    let items = cached_catalog.lock().unwrap().clone();
    if items.is_empty() {
        show_error(ui, "No catalog items available for random playback");
        return;
    }
    let current_id = current_item
        .lock()
        .unwrap()
        .as_ref()
        .map(|i| i.id.clone())
        .unwrap_or_default();
    let candidates: Vec<&TvItem> = items.iter().filter(|i| i.id != current_id).collect();
    let target = if !candidates.is_empty() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let idx = ((nanos ^ (std::process::id() as u128)) % (candidates.len() as u128)) as usize;
        candidates[idx].clone()
    } else {
        items[0].clone()
    };

    start_item_playback(
        ui,
        player,
        target,
        headers,
        credential_store,
        local_store,
        jellyfin,
        reporter,
        session_ctrl,
        current_item,
        phash_coord,
    );
}

#[allow(dead_code)]
pub fn play_random_catalog_item_ctx(ui: &MainWindow, ctx: &AppContext) {
    let Some(player) = &ctx.player else {
        show_error(ui, "libmpv is not available");
        return;
    };
    play_random_catalog_item(
        ui,
        player,
        &ctx.headers,
        &ctx.credential_store,
        &ctx.local_store,
        &ctx.jellyfin,
        &ctx.reporter,
        &ctx.session_ctrl,
        &ctx.current_playing_item,
        &ctx.cached_catalog,
        &ctx.phash_coord,
    );
}

#[cfg(windows)]
pub fn pick_media_file() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Open media")
        .add_filter(
            "Media",
            &[
                "3g2", "3gp", "asf", "avi", "divx", "f4v", "flv", "m2ts", "m2v", "m3u", "m3u8",
                "m4v", "mkv", "mov", "mp4", "mpe", "mpeg", "mpg", "mts", "ogm", "ogv", "rm",
                "rmvb", "ts", "vob", "webm", "wmv",
            ],
        )
        .pick_file()
        .map(|path| path.to_string_lossy().into_owned())
}

#[cfg(not(windows))]
pub fn pick_media_file() -> Option<String> {
    None
}

#[cfg(windows)]
pub fn export_file_dialog(default_name: &str, ext: &str) -> String {
    rfd::FileDialog::new()
        .set_file_name(default_name)
        .add_filter(ext, &[ext])
        .save_file()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| default_name.to_string())
}

#[cfg(not(windows))]
pub fn export_file_dialog(default_name: &str, _ext: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        let downloads = PathBuf::from(&home).join("Downloads");
        if downloads.is_dir() {
            return downloads.join(default_name).to_string_lossy().into_owned();
        }
        return PathBuf::from(&home)
            .join(default_name)
            .to_string_lossy()
            .into_owned();
    }
    default_name.to_string()
}

#[cfg(windows)]
pub fn pick_config_file() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Import Configuration")
        .add_filter("JSON", &["json"])
        .pick_file()
        .map(|path| path.to_string_lossy().into_owned())
}

#[cfg(not(windows))]
pub fn pick_config_file() -> Option<String> {
    let candidate = PathBuf::from("adult-hub-config.json");
    if candidate.exists() {
        Some(candidate.to_string_lossy().into_owned())
    } else {
        None
    }
}
