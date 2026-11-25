use crate::MainWindow;
use crate::context::AppContext;
use crate::phash::PhashCoordinator;
use crate::reporter::PlaybackReporter;
use crate::session::PlaybackSessionController;
use adult_hub_client::TvItem;
use adult_hub_player::{PlaybackStats, Player};
use adult_hub_storage::{CredentialStore, LocalStore};
use slint::ComponentHandle;
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
    ui.set_is_switching_movie(false);
    ui.set_is_changing_quality(false);
    if ui.get_active_screen() == "player" {
        ui.set_player_media_loaded(false);
        ui.set_player_controls_visible(true);
        ui.set_player_more_controls_visible(false);
        ui.set_player_error_message(msg.clone());
        ui.set_player_error_details("".into());
        ui.set_player_error_details_visible(false);
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

pub fn show_playback_error(
    ui: &MainWindow,
    short_message: impl Into<slint::SharedString>,
    detail: impl Into<slint::SharedString>,
) {
    let short = short_message.into();
    let det = detail.into();
    crate::diagnostics::GLOBAL_LOG.e("Playback", &format!("{short}: {det}"));
    ui.set_is_switching_movie(false);
    ui.set_is_changing_quality(false);
    if ui.get_active_screen() == "player" {
        ui.set_player_media_loaded(false);
        ui.set_player_controls_visible(true);
        ui.set_player_more_controls_visible(false);
        ui.set_player_error_message(short.clone());
        ui.set_player_error_details(det);
        ui.set_player_error_details_visible(false);
    } else {
        ui.set_toast_message(short.clone());
        ui.set_toast_type("error".into());
        ui.set_toast_action_text("".into());
        ui.set_toast_visible(true);
    }
    ui.set_status(short);
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
    ui.set_player_volume_flyout_open(false);
    ui.set_download_panel_open(false);
    ui.set_player_has_quality_alternatives(false);
    ui.set_player_quality_panel_open(false);
    ui.set_player_quality_subpage_open(false);
    ui.set_player_selected_quality_id("original".into());
    ui.set_player_selected_quality_label("Original".into());
    ui.set_player_quality_options(slint::ModelRc::default());
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
    let clean_path = adult_hub_player::sanitize_media_url(&path);
    ui.set_status(format!("Loading: {clean_path}").into());
    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
    if let Err(error) = player.load_with_start(&path, headers, None) {
        // mpv never accepted the file: do not leave the UI claiming playback.
        ui.set_playing(false);
        ui.set_has_active_media(false);
        ui.set_player_state("failed".into());
        show_error(ui, error.to_string());
    }
}

/// Save the current item's final position and release playback when leaving the player screen.
pub fn format_player_subtitle(item: &TvItem) -> String {
    let mut parts = Vec::new();
    if let Some(ticks) = item.runtime_ticks {
        let secs = (ticks / 10_000_000) as f64;
        if secs > 0.0 {
            parts.push(format_time(secs));
        }
    }
    if let Some(year) = item.year {
        parts.push(year.to_string());
    }
    if let Some(studio) = item.studios.first() {
        if !studio.trim().is_empty() {
            parts.push(studio.clone());
        }
    } else if let Some(provider) = &item.source_provider {
        if !provider.trim().is_empty() {
            parts.push(provider.clone());
        }
    }
    if let (Some(w), Some(h)) = (item.width, item.height) {
        if h >= 2160 {
            parts.push("4K".to_string());
        } else if h >= 1080 {
            parts.push("1080p".to_string());
        } else if h >= 720 {
            parts.push("720p".to_string());
        } else if h > 0 {
            parts.push(format!("{w}x{h}"));
        }
    }
    parts.join(" • ")
}

pub fn stop_active_playback_for_navigation(
    ui: &MainWindow,
    player: Option<&Player>,
    reporter: &PlaybackReporter,
    session_ctrl: &PlaybackSessionController,
    current_item: &Arc<Mutex<Option<TvItem>>>,
) {
    session_ctrl.next_request_id();
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

    if let Some(item) = current_item
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        let is_ended = ui.get_playback_ended_visible();
        let is_completed =
            PlaybackSessionController::is_playback_completed(position_ms, duration_ms, is_ended);
        session_ctrl.checkpoint_progress(&item, position_ms, duration_ms, is_completed);
        reporter.report_api_progress(&item.id, position_ms, duration_ms, is_completed);
    }
    session_ctrl.next_request_id();
    if let Some(player) = player {
        let _ = player.stop();
    }

    ui.set_has_active_media(false);
    ui.set_is_switching_movie(false);
    ui.set_player_subtitle("".into());
    ui.set_playing(false);
    ui.set_player_controls_visible(true);
    ui.set_player_more_controls_visible(false);
    ui.set_player_volume_flyout_open(false);
    ui.set_download_panel_open(false);
    ui.set_player_has_quality_alternatives(false);
    ui.set_player_quality_panel_open(false);
    ui.set_player_quality_subpage_open(false);
    ui.set_player_selected_quality_id("original".into());
    ui.set_player_selected_quality_label("Original".into());
    ui.set_player_quality_options(slint::ModelRc::default());
    ui.set_player_media_loaded(false);
    ui.set_player_title("".into());
    ui.set_position(0.0);
    ui.set_duration(0.0);
    ui.set_position_text("0:00".into());
    ui.set_duration_text("0:00".into());
    ui.set_playback_ended_visible(false);
    ui.set_autoplay_countdown_sec(0);
    ui.set_player_state("idle".into());
    ui.set_player_loading_message("".into());
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
    mut item: TvItem,
    headers: &[String],
    credential_store: &Arc<CredentialStore>,
    local_store: &Arc<LocalStore>,
    reporter: &Arc<PlaybackReporter>,
    session_ctrl: &Arc<PlaybackSessionController>,
    current_item: &Arc<Mutex<Option<TvItem>>>,
    phash_coord: &Arc<PhashCoordinator>,
    resume_choice: Option<bool>,
) {
    session_ctrl.next_request_id();

    // 1. Persist outgoing video's progress and stop session
    let prev = current_item
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(prev) = prev {
        let pos = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
        let dur = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
        let is_completed = PlaybackSessionController::is_playback_completed(pos, dur, false);
        session_ctrl.checkpoint_progress(&prev, pos, dur, is_completed);
        reporter.report_api_progress(&prev.id, pos, dur, is_completed);
    }

    // 2. Resolve stream URL
    let session = credential_store.get().ok().flatten();
    let resolver_url =
        crate::context::AppContext::resolve_effective_resolver_url(local_store, credential_store);

    let is_direct_media = item.is_direct_media() || item.id == "sample" || item.id == "sample2";
    let (stream_url, _play_method, required_headers) = if is_direct_media {
        match PlaybackSessionController::resolve_stream_url(
            &item,
            session.as_ref().map(|s| s.server_url.as_str()),
            resolver_url.as_deref(),
            session.as_ref().map(|s| s.access_token.as_str()),
        ) {
            Ok((url, method)) => (url, method, Vec::new()),
            Err(err) => {
                *current_item
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(item.clone());
                ui.set_has_active_media(true);
                ui.set_player_title(crate::catalog::display_title(&item).into());
                ui.set_player_subtitle(format_player_subtitle(&item).into());
                ui.set_active_screen("player".into());
                ui.invoke_focus_keys();
                show_error(ui, err.clone());
                ui.set_toast_message(err.into());
                ui.set_toast_type("error".into());
                ui.set_toast_action_text("".into());
                ui.set_toast_visible(true);
                return;
            }
        }
    } else {
        let Some(session) = session.as_ref() else {
            ui.set_player_state("failed".into());
            show_error(ui, "Sign in to Adult Hub before playing this item.");
            return;
        };
        let client = match adult_hub_client::AdultHubClient::new(&session.server_url) {
            Ok(c) => c.with_token(&session.access_token),
            Err(err) => {
                ui.set_player_state("failed".into());
                show_error(ui, format!("Failed to initialize Adult Hub client: {err}"));
                return;
            }
        };
        let descriptor = match resolve_playback_descriptor(&client, &mut item) {
            Ok(d) => d,
            Err(err) => {
                let safe_msg = format!(
                    "Could not resolve Adult Hub playback: {}",
                    format_safe_adult_hub_error(&err)
                );
                ui.set_player_state("failed".into());
                show_error(ui, safe_msg);
                return;
            }
        };
        match resolve_adult_hub_stream_target(&client, &descriptor, None) {
            Ok(res) => res,
            Err(err) => {
                ui.set_player_state("failed".into());
                show_error(ui, format!("Could not resolve Adult Hub playback: {err}"));
                return;
            }
        }
    };

    let stream_headers = route_playback_headers(headers, &required_headers);

    // 3. Determine resume position
    let duration_ms = item.runtime_ticks.map(|t| (t / 10_000) as u64).unwrap_or(0);
    let mut start_pos_secs = 0.0;

    let mut server_progress = None;
    if let Some(session) = session.as_ref() {
        if let Ok(client) = adult_hub_client::AdultHubClient::new(&session.server_url) {
            let authed_client = client.with_token(&session.access_token);
            if let Ok(progress) = authed_client.get_watch_progress(&item.id) {
                server_progress = progress;
            }
        }
    }

    let calculated_resume_ms =
        resolve_playback_resume_ms(server_progress.as_ref(), local_store, session_ctrl, &item);

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
            if let Some(resume_ms) = calculated_resume_ms {
                if PlaybackSessionController::should_prompt_resume(resume_ms, duration_ms) {
                    start_pos_secs = resume_ms as f64 / 1000.0;
                }
            }
        }
    }

    // 4. Update UI & current item
    *current_item
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(item.clone());
    ui.set_has_active_media(true);
    ui.set_playing(true);
    ui.set_player_state("loading".into());
    ui.set_player_is_favorite(item.is_favorite);
    ui.set_player_favorite_enabled(!item.is_telegram() && uuid::Uuid::parse_str(&item.id).is_ok());
    ui.set_player_error_message(String::new().into());
    ui.set_player_controls_visible(true);
    ui.set_player_more_controls_visible(false);
    ui.set_player_volume_flyout_open(false);
    ui.set_download_panel_open(false);
    ui.set_player_has_quality_alternatives(false);
    ui.set_player_quality_panel_open(false);
    ui.set_player_quality_subpage_open(false);
    ui.set_player_selected_quality_id("original".into());
    ui.set_player_selected_quality_label("Original".into());
    ui.set_player_quality_options(slint::ModelRc::default());
    ui.set_player_media_loaded(false);
    ui.set_player_title(crate::catalog::display_title(&item).into());
    ui.set_player_subtitle(format_player_subtitle(&item).into());
    ui.set_player_is_telegram_stream(item.is_telegram());
    ui.set_player_has_quality_alternatives(false);
    ui.set_player_quality_subpage_open(false);
    ui.set_player_selected_quality_id("original".into());
    ui.set_player_selected_quality_label("Original".into());
    ui.set_player_quality_options(slint::ModelRc::default());
    player.set_quality_variant(None);
    ui.set_url_text(stream_url.clone().into());
    ui.set_status(format!("Loading: {}", item.title).into());
    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
    ui.set_active_screen("player".into());
    ui.invoke_focus_keys();

    // 5. Start player & session reporting
    let start_pos_opt = if start_pos_secs > 0.0 {
        Some(start_pos_secs)
    } else {
        None
    };
    let path = resolve_media_input(&stream_url);
    reset_playback_speed(ui, player);
    if let Err(e) = player.load_with_start(&path, &stream_headers, start_pos_opt) {
        // mpv never accepted the file: drop the optimistic playback flags but
        // keep the item so the error screen's retry can start it again.
        ui.set_playing(false);
        ui.set_has_active_media(false);
        ui.set_player_state("failed".into());
        show_playback_error(
            ui,
            if start_pos_secs > 0.0 {
                "Could not resume video from saved position"
            } else {
                "Could not load video stream"
            },
            e.to_string(),
        );
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

    // 6. Enqueue pHash video matching if enabled
    let phash_enabled = local_store
        .get_setting("phash_on_playback")
        .ok()
        .flatten()
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    if phash_enabled && (item.source_provider.is_some() || item.is_telegram()) {
        phash_coord.enqueue_generation(&item, None);
    }
}

pub fn format_safe_adult_hub_error(err: &adult_hub_client::AdultHubError) -> String {
    match err {
        adult_hub_client::AdultHubError::Unauthorized { message, .. } => {
            format!("Session unauthorized: {message}")
        }
        adult_hub_client::AdultHubError::NotFound(msg) => format!("Item not found: {msg}"),
        adult_hub_client::AdultHubError::RateLimited(msg) => format!("Rate limited: {msg}"),
        adult_hub_client::AdultHubError::OriginMismatch { expected, actual } => {
            format!("Origin mismatch: expected {expected}, got {actual}")
        }
        adult_hub_client::AdultHubError::InvalidUrl(_) => "Invalid stream URL".to_string(),
        adult_hub_client::AdultHubError::Api {
            status, message, ..
        } => {
            format!("API error (HTTP {status}): {message}")
        }
        adult_hub_client::AdultHubError::Http(e) => {
            if let Some(status) = e.status() {
                format!("HTTP error {status}")
            } else {
                "Network request failed".to_string()
            }
        }
        adult_hub_client::AdultHubError::Serialization(_) => "Invalid response format".to_string(),
        adult_hub_client::AdultHubError::Conflict(msg) => format!("Conflict: {msg}"),
        adult_hub_client::AdultHubError::PayloadTooLarge { limit } => {
            format!("Response too large (exceeded {limit} bytes)")
        }
        adult_hub_client::AdultHubError::Io(e) => {
            format!("I/O error: {e}")
        }
    }
}

/// Resolve a playback descriptor for the item. Local resume/history rows written
/// by older releases may key a Telegram item by a legacy hash instead of the
/// canonical catalog id. When the server returns 404, retry once by exact title
/// (and closest duration) so those items remain playable.
fn resolve_playback_descriptor(
    client: &adult_hub_client::AdultHubClient,
    item: &mut TvItem,
) -> std::result::Result<adult_hub_client::PlaybackDescriptorDto, adult_hub_client::AdultHubError> {
    match client.get_playback_descriptor(&item.id) {
        Ok(descriptor) => Ok(descriptor),
        Err(err @ adult_hub_client::AdultHubError::NotFound(_)) => {
            let title = item.title.trim();
            if title.is_empty() {
                return Err(err);
            }
            let Ok(response) = client.get_videos(&adult_hub_client::CatalogVideosQuery {
                offset: Some(0),
                limit: Some(10),
                q: Some(title),
                ..Default::default()
            }) else {
                return Err(err);
            };
            let duration = item
                .runtime_ticks
                .map(|ticks| (ticks / 10_000_000).max(0) as u64)
                .unwrap_or(0);
            let Some(candidate) = response.items.into_iter().find(|candidate| {
                candidate.title.trim().eq_ignore_ascii_case(title)
                    && (duration == 0
                        || candidate.duration_seconds == 0
                        || candidate.duration_seconds.abs_diff(duration) <= 5)
            }) else {
                return Err(err);
            };
            crate::log_debug(&format!(
                "playback: remapped stale item {} to canonical {} by title",
                item.id, candidate.id
            ));
            item.id = candidate.id.clone();
            item.title = candidate.title;
            if item.source_provider.is_none() {
                item.source_provider = Some("adult_hub".into());
            }
            client.get_playback_descriptor(&item.id)
        }
        Err(err) => Err(err),
    }
}

pub fn resolve_adult_hub_stream_target(
    client: &adult_hub_client::AdultHubClient,
    descriptor: &adult_hub_client::PlaybackDescriptorDto,
    selected_variant_id: Option<&str>,
) -> std::result::Result<(String, adult_hub_client::PlayMethod, Vec<String>), String> {
    let active_id = selected_variant_id.unwrap_or("original");
    let stream_path = if active_id != "original" {
        let rendition = descriptor.rendition_by_id(active_id).ok_or_else(|| {
            format!("Selected rendition '{active_id}' not found in playback descriptor")
        })?;
        rendition
            .stream_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                format!("Selected rendition '{active_id}' does not have a stream path")
            })?
    } else {
        descriptor.default_stream_path().ok_or_else(|| {
            "No stream path available in Adult Hub playback descriptor".to_string()
        })?
    };

    let trimmed = stream_path.trim();
    if trimmed.is_empty() {
        return Err("Stream path cannot be empty".to_string());
    }
    if trimmed.starts_with("//") {
        return Err("Protocol-relative stream URLs are not allowed".to_string());
    }

    let stream_url = client
        .resolve_stream_url(trimmed)
        .map_err(|e| format!("Invalid stream URL: {e}"))?;

    // `resolve_stream_url` already enforces the same scheme/host/port as the
    // client's base URL. Do not re-compare serialized origins here: url
    // omits default ports (e.g. :443), while ServiceUrl::origin keeps them,
    // which produced a false mismatch on HTTPS servers.

    let mut headers = Vec::new();
    if let Some(auth) = client.auth_header_value() {
        headers.push(format!("Authorization: {auth}"));
    } else if let Some(token) = client.token() {
        if !token.is_empty() {
            headers.push(format!("Authorization: Bearer {token}"));
        }
    }

    Ok((
        stream_url,
        adult_hub_client::PlayMethod::DirectPlay,
        headers,
    ))
}

pub fn route_playback_headers(base_headers: &[String], required_headers: &[String]) -> Vec<String> {
    let mut stream_headers = Vec::new();
    for h in base_headers {
        if !h.to_ascii_lowercase().starts_with("authorization:") {
            stream_headers.push(h.clone());
        }
    }
    for h in required_headers {
        stream_headers.push(h.clone());
    }
    stream_headers
}

pub fn resolve_playback_resume_ms(
    server_progress: Option<&adult_hub_client::WatchProgressDto>,
    local_store: &adult_hub_storage::LocalStore,
    session_ctrl: &PlaybackSessionController,
    item: &TvItem,
) -> Option<u64> {
    if let Some(progress) = server_progress {
        if progress.is_played {
            None
        } else if progress.position_seconds > 0 {
            Some(progress.position_seconds * 1000)
        } else {
            None
        }
    } else {
        PlaybackSessionController::resolve_item_resume_position(local_store, session_ctrl, item)
    }
}

pub fn start_item_playback_ctx(ui: &MainWindow, ctx: &AppContext, item: TvItem) {
    start_item_playback_with_resume_ctx(ui, ctx, item, None);
}

pub fn start_item_playback_with_resume_ctx(
    ui: &MainWindow,
    ctx: &AppContext,
    mut item: TvItem,
    resume_choice: Option<bool>,
) {
    let Some(player) = ctx.player.clone() else {
        show_error(ui, "libmpv is not available");
        return;
    };

    // Ignore duplicate dispatch if this exact item is already resolving or loading
    if (ui.get_player_state() == "resolving" || ui.get_player_state() == "loading")
        && ctx
            .current_playing_item
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|cur| cur.id == item.id)
    {
        return;
    }

    let request_id = ctx.session_ctrl.next_request_id();
    crate::log_debug(&format!(
        "playback req={request_id}: dispatch direct={} telegram={}",
        item.is_direct_media(),
        item.is_telegram()
    ));

    // 1. Checkpoint outgoing video's progress BEFORE touching playback state
    let pos = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
    let dur = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
    let is_same_item = ctx
        .current_playing_item
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|cur| cur.id == item.id);
    let preserve_quality = is_same_item;

    let prev_item = ctx
        .current_playing_item
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();

    if let Some(prev) = prev_item {
        let is_completed = PlaybackSessionController::is_playback_completed(pos, dur, false);
        if prev.id != item.id {
            ctx.session_ctrl
                .checkpoint_progress(&prev, pos, dur, is_completed);
            ctx.reporter
                .report_api_progress(&prev.id, pos, dur, is_completed);
        }
    }

    // Pause outgoing playback without stopping mpv pipeline or discarding buffer
    let _ = player.set_pause(true);

    let initial_quality_id = ctx
        .active_quality_id
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let selected_quality = if preserve_quality && initial_quality_id != "original" {
        initial_quality_id
    } else {
        *ctx.active_quality_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = "original".to_string();
        "original".to_string()
    };

    // 2. Immediately update UI to show player screen and loading/resolving overlay
    *ctx.current_playing_item
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(item.clone());
    ui.set_has_active_media(true);
    ui.set_playing(true);
    ui.set_player_is_favorite(item.is_favorite);
    ui.set_player_favorite_enabled(!item.is_telegram() && item.source_provider.is_some());
    ui.set_player_error_message(String::new().into());
    ui.set_toast_visible(false); // Clear any stale error toast
    ui.set_player_controls_visible(true);
    ui.set_player_more_controls_visible(false);
    ui.set_player_volume_flyout_open(false);
    ui.set_download_panel_open(false);
    ui.set_player_has_quality_alternatives(false);
    ui.set_player_quality_panel_open(false);
    ui.set_player_quality_subpage_open(false);
    ui.set_player_selected_quality_id(selected_quality.clone().into());
    ui.set_player_selected_quality_label(if selected_quality == "original" {
        "Original".into()
    } else {
        selected_quality.clone().into()
    });
    ui.set_player_quality_options(slint::ModelRc::default());
    ui.set_player_media_loaded(false);
    ui.set_player_state("resolving".into());
    ui.set_player_loading_message("Resolving stream address…".into());
    ui.set_player_title(crate::catalog::display_title(&item).into());
    ui.set_player_subtitle(format_player_subtitle(&item).into());
    ui.set_status(format!("Preparing: {}", item.title).into());
    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
    ui.set_player_is_telegram_stream(item.is_telegram());
    ui.set_player_has_quality_alternatives(false);
    ui.set_player_quality_subpage_open(false);
    *ctx.current_video_variants
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Vec::new();
    player.set_quality_variant(None);

    if ui.get_active_screen() != "player" {
        if ui.get_active_screen() == "browse" {
            ui.invoke_cache_catalog_view();
        }
        ctx.push_nav_destination(crate::handlers::browsing::current_nav_destination(ui, ctx));
    }
    ui.set_active_screen("player".into());
    ui.invoke_focus_keys();

    // 3. Resolve stream URL in background thread
    let weak_ui = ui.as_weak();
    let ctx_clone = ctx.clone();
    let headers = ctx.headers.clone();

    std::thread::spawn(move || {
        let resolve_started = std::time::Instant::now();
        let session = ctx_clone.credential_store.get().ok().flatten();
        let resolver_url = ctx_clone.get_resolver_url();

        let mut server_progress = None;
        let resolution_result = {
            let Some(session) = session.as_ref() else {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak_ui.upgrade() {
                        if !ctx_clone.session_ctrl.is_request_current(request_id) {
                            return;
                        }
                        ui.set_player_state("failed".into());
                        ui.set_is_changing_quality(false);
                        ui.set_is_switching_movie(false);
                        show_error(&ui, "Sign in to Adult Hub before playing this item.");
                        ui.set_toast_message(
                            "Sign in to Adult Hub before playing this item.".into(),
                        );
                        ui.set_toast_type("error".into());
                        ui.set_toast_action_text("".into());
                        ui.set_toast_visible(true);
                    }
                });
                return;
            };

            let client = match adult_hub_client::AdultHubClient::new(&session.server_url) {
                Ok(c) => c.with_token(&session.access_token),
                Err(err) => {
                    let msg = format!("Failed to initialize Adult Hub client: {err}");
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak_ui.upgrade() {
                            if !ctx_clone.session_ctrl.is_request_current(request_id) {
                                return;
                            }
                            ui.set_player_state("failed".into());
                            ui.set_is_changing_quality(false);
                            ui.set_is_switching_movie(false);
                            show_error(&ui, msg.clone());
                            ui.set_toast_message(msg.into());
                            ui.set_toast_type("error".into());
                            ui.set_toast_action_text("".into());
                            ui.set_toast_visible(true);
                        }
                    });
                    return;
                }
            };

            if let Ok(progress) = client.get_watch_progress(&item.id) {
                server_progress = progress;
            }

            let descriptor = match resolve_playback_descriptor(&client, &mut item) {
                Ok(d) => d,
                Err(err) => {
                    let is_unauth = err.is_unauthorized();
                    let safe_err = format_safe_adult_hub_error(&err);
                    let msg = format!("Could not resolve Adult Hub playback: {safe_err}");
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak_ui.upgrade() {
                            if !ctx_clone.session_ctrl.is_request_current(request_id) {
                                return;
                            }
                            ui.set_player_state("failed".into());
                            ui.set_is_changing_quality(false);
                            ui.set_is_switching_movie(false);
                            show_error(&ui, msg.clone());
                            if is_unauth {
                                ui.set_is_logged_in(false);
                                ui.set_login_modal_visible(true);
                                ui.set_login_modal_message(
                                    "Session expired (401 Unauthorized). Please sign in again."
                                        .into(),
                                );
                                ui.set_toast_message("Session expired. Please reconnect.".into());
                                ui.set_toast_type("warning".into());
                                ui.set_toast_action_text("Sign In".into());
                                ui.set_toast_visible(true);
                            } else {
                                ui.set_toast_message(msg.into());
                                ui.set_toast_type("error".into());
                                ui.set_toast_action_text("".into());
                                ui.set_toast_visible(true);
                            }
                        }
                    });
                    return;
                }
            };

            let renditions = descriptor.renditions.clone();
            if !renditions.is_empty() {
                let variants: Vec<adult_hub_client::VideoVariantDto> = renditions
                    .iter()
                    .map(|r| adult_hub_client::VideoVariantDto {
                        id: r.id.clone(),
                        is_original: r.is_default,
                        width: r.width,
                        height: r.height,
                        codec: None,
                        mime_type: r.mime_type.clone(),
                        file_size: r.file_size_bytes,
                        duration: descriptor.duration_seconds,
                        bitrate: None,
                        estimated_average_bitrate: None,
                    })
                    .collect();

                let weak_ui_q = weak_ui.clone();
                let variants_q = variants.clone();
                let active_q = selected_quality.clone();
                let player_q = ctx_clone.player.clone();
                let ctx_q = ctx_clone.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak_ui_q.upgrade() else {
                        return;
                    };
                    if !ctx_q.session_ctrl.is_request_current(request_id) {
                        return;
                    }

                    *ctx_q
                        .current_video_variants
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = variants_q.clone();

                    if variants_q.len() > 1 {
                        let opts = format_variant_options(&variants_q, &active_q);
                        let active_label = opts
                            .iter()
                            .find(|o| o.id == active_q)
                            .map(|o| o.label.to_string())
                            .unwrap_or_else(|| "Original".to_string());
                        ui.set_player_has_quality_alternatives(true);
                        ui.set_player_selected_quality_label(active_label.clone().into());
                        if let Some(p) = player_q {
                            p.set_quality_variant(Some(active_label));
                        }
                        update_slint_quality_options(&ui, &opts);
                    }
                });
            }

            resolve_adult_hub_stream_target(&client, &descriptor, Some(&selected_quality))
        };

        let (stream_url, _play_method, required_headers) = match resolution_result {
            Ok(res) => {
                crate::log_debug(&format!(
                    "playback req={request_id}: resolved in {} ms, method={:?}, headers={}",
                    resolve_started.elapsed().as_millis(),
                    res.1,
                    res.2.len()
                ));
                res
            }
            Err(err) => {
                crate::log_debug(&format!(
                    "playback req={request_id}: resolve failed after {} ms: {err}",
                    resolve_started.elapsed().as_millis()
                ));
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak_ui.upgrade() {
                        if !ctx_clone.session_ctrl.is_request_current(request_id) {
                            return;
                        }
                        if item.is_telegram()
                            && (resolver_url.is_none()
                                || resolver_url.as_deref().unwrap_or("").trim().is_empty())
                        {
                            ui.set_player_state("failed".into());
                            ui.set_resolver_prompt_dialog_visible(true);
                            ui.set_toast_message(
                                "Resolver URL required to stream Telegram videos. Click here to configure.".into(),
                            );
                            ui.set_toast_type("warning".into());
                            ui.set_toast_action_text("Configure".into());
                            ui.set_toast_visible(true);
                        } else {
                            ui.set_player_state("failed".into());
                            show_error(&ui, err.clone());
                            ui.set_toast_message(err.into());
                            ui.set_toast_type("error".into());
                            ui.set_toast_action_text("".into());
                            ui.set_toast_visible(true);
                        }
                    }
                });
                return;
            }
        };

        let stream_headers = route_playback_headers(&headers, &required_headers);

        // Determine resume position
        let duration_ms = item.runtime_ticks.map(|t| (t / 10_000) as u64).unwrap_or(0);
        let mut start_pos_secs = 0.0;

        let calculated_resume_ms = resolve_playback_resume_ms(
            server_progress.as_ref(),
            &ctx_clone.local_store,
            &ctx_clone.session_ctrl,
            &item,
        );

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
                if let Some(resume_ms) = calculated_resume_ms {
                    if PlaybackSessionController::should_prompt_resume(resume_ms, duration_ms) {
                        start_pos_secs = resume_ms as f64 / 1000.0;
                    }
                }
            }
        }

        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak_ui.upgrade() {
                if !ctx_clone.session_ctrl.is_request_current(request_id) {
                    return;
                }
                ui.set_player_state("loading".into());
                ui.set_player_loading_message("Opening media stream…".into());
                ui.set_url_text(stream_url.clone().into());
                ui.set_status(format!("Loading: {}", item.title).into());

                let start_pos_opt = if start_pos_secs > 0.0 {
                    Some(start_pos_secs)
                } else {
                    None
                };
                *ctx_clone
                    .pending_resume_position
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = start_pos_opt;

                let path = resolve_media_input(&stream_url);
                reset_playback_speed(&ui, &player);
                crate::log_debug(&format!(
                    "playback req={request_id}: handing file to mpv, resume_pos={start_pos_secs:.1}s"
                ));
                if let Err(e) = player.load_with_start(&path, &stream_headers, start_pos_opt) {
                    // mpv never accepted the file: drop the optimistic
                    // playback flags but keep the item so the error screen's
                    // retry can start it again.
                    crate::log_debug(&format!(
                        "playback req={request_id}: mpv rejected load: {e}"
                    ));
                    ui.set_playing(false);
                    ui.set_has_active_media(false);
                    ui.set_player_state("failed".into());
                    show_playback_error(
                        &ui,
                        if start_pos_secs > 0.0 {
                            "Could not resume video from saved position"
                        } else {
                            "Could not load video stream"
                        },
                        e.to_string(),
                    );
                    return;
                }
                let _ = player.set_pause(false);
                ui.set_playing(true);
                crate::log_debug(&format!("playback req={request_id}: mpv accepted load"));

                if !item.is_direct_media() {
                    if let Ok(item_json) = serde_json::to_string(&item) {
                        let _ = ctx_clone
                            .local_store
                            .record_watch_history(&item.id, &item_json);
                    }
                }

                let phash_enabled = ctx_clone
                    .local_store
                    .get_setting("phash_on_playback")
                    .ok()
                    .flatten()
                    .map(|v| v == "true" || v == "1")
                    .unwrap_or(false);
                if phash_enabled && (item.source_provider.is_some() || item.is_telegram()) {
                    ctx_clone.phash_coord.enqueue_generation(&item, None);
                }
            }
        });
    });
}

#[allow(dead_code)]
pub fn play_random_catalog_item(
    ui: &MainWindow,
    _player: &Player,
    _headers: &[String],
    _credential_store: &Arc<CredentialStore>,
    _local_store: &Arc<LocalStore>,
    _reporter: &Arc<PlaybackReporter>,
    _session_ctrl: &Arc<PlaybackSessionController>,
    _current_item: &Arc<Mutex<Option<TvItem>>>,
    _cached_catalog: &Arc<Mutex<Vec<TvItem>>>,
    _phash_coord: &Arc<PhashCoordinator>,
) {
    ui.invoke_play_random();
}

#[allow(dead_code)]
pub fn play_random_catalog_item_ctx(ui: &MainWindow, _ctx: &AppContext) {
    ui.invoke_play_random();
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

pub fn normalize_codec_name(raw: &str) -> String {
    let lower = raw.trim().to_lowercase();
    match lower.as_str() {
        "h264" | "avc" | "avc1" | "x264" | "h.264" => "H.264".to_string(),
        "hevc" | "h265" | "x265" | "hev1" | "hvc1" | "h.265" => "HEVC".to_string(),
        "av1" | "av01" => "AV1".to_string(),
        "vp9" | "vp09" => "VP9".to_string(),
        "vp8" => "VP8".to_string(),
        "" => String::new(),
        other => other.to_uppercase(),
    }
}

pub fn format_variant_options(
    variants: &[adult_hub_client::VideoVariantDto],
    active_id: &str,
) -> Vec<crate::VideoQualityOption> {
    use std::collections::HashMap;
    let mut height_counts: HashMap<Option<u32>, usize> = HashMap::new();
    for v in variants {
        *height_counts.entry(v.height).or_insert(0) += 1;
    }

    variants
        .iter()
        .map(|v| {
            let has_duplicate = height_counts.get(&v.height).copied().unwrap_or(0) > 1;
            let label = v.display_label(has_duplicate);
            let res = v.resolution_label();
            let codec_norm = v
                .codec
                .as_deref()
                .map(normalize_codec_name)
                .unwrap_or_default();
            let size_text = v
                .file_size
                .map(adult_hub_client::format_media_size)
                .unwrap_or_default();
            let is_selected = v.id == active_id;

            crate::VideoQualityOption {
                id: v.id.clone().into(),
                label: label.into(),
                resolution: res.into(),
                details: codec_norm.clone().into(),
                codec: codec_norm.into(),
                size_text: size_text.into(),
                is_original: v.is_original,
                is_selected,
            }
        })
        .collect()
}

pub fn update_slint_quality_options(ui: &MainWindow, options: &[crate::VideoQualityOption]) {
    let model = std::rc::Rc::new(slint::VecModel::from(options.to_vec()));
    ui.set_player_quality_options(model.into());
}

pub fn switch_video_quality(ui: &MainWindow, ctx: &AppContext, target_variant_id: String) {
    let Some(player) = ctx.player.clone() else {
        return;
    };
    let current_variant = ctx
        .active_quality_id
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if current_variant == target_variant_id {
        return;
    }
    if ui.get_is_changing_quality() {
        return;
    }

    let item = ctx
        .current_playing_item
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let Some(item) = item else {
        return;
    };

    // Capture complete player state before touching or preparing new stream
    let current_pos_secs = player.position().unwrap_or(0.0);
    let was_paused = player.is_paused();
    let current_volume = player.volume().unwrap_or(100.0);
    let was_muted = player.is_muted();
    let current_speed = player.get_f64("speed").unwrap_or(1.0);
    let previous_stream_url = ui.get_url_text().to_string();

    ui.set_player_quality_panel_open(false);
    ui.set_player_quality_subpage_open(false);
    ui.set_player_more_controls_visible(false);
    ui.set_player_volume_flyout_open(false);
    ui.set_download_panel_open(false);
    ui.set_is_changing_quality(true);
    ui.set_player_loading_message("Changing quality…".into());

    let quality_req_id = ctx.session_ctrl.next_request_id();
    let weak_ui = ui.as_weak();
    let ctx_clone = ctx.clone();
    let headers = ctx.headers.clone();
    let player_clone = player.clone();

    std::thread::spawn(move || {
        let session = ctx_clone.credential_store.get().ok().flatten();
        let mut quality_headers = Vec::new();

        let resolution_result = {
            let Some(session) = session.as_ref() else {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak_ui.upgrade() {
                        if !ctx_clone.session_ctrl.is_request_current(quality_req_id) {
                            return;
                        }
                        ui.set_is_changing_quality(false);
                        ui.set_toast_message("Session expired. Please reconnect.".into());
                        ui.set_toast_type("warning".into());
                        ui.set_toast_action_text("Sign In".into());
                        ui.set_toast_visible(true);
                    }
                });
                return;
            };
            let client = match adult_hub_client::AdultHubClient::new(&session.server_url) {
                Ok(c) => c.with_token(&session.access_token),
                Err(e) => {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak_ui.upgrade() {
                            if !ctx_clone.session_ctrl.is_request_current(quality_req_id) {
                                return;
                            }
                            ui.set_is_changing_quality(false);
                            ui.set_toast_message(format!("Could not switch quality: {e}").into());
                            ui.set_toast_type("error".into());
                            ui.set_toast_action_text("".into());
                            ui.set_toast_visible(true);
                        }
                    });
                    return;
                }
            };
            let descriptor = match client.get_playback_descriptor(&item.id) {
                Ok(d) => d,
                Err(e) => {
                    let safe_msg = format!(
                        "Could not switch quality: {}",
                        format_safe_adult_hub_error(&e)
                    );
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak_ui.upgrade() {
                            if !ctx_clone.session_ctrl.is_request_current(quality_req_id) {
                                return;
                            }
                            ui.set_is_changing_quality(false);
                            ui.set_toast_message(safe_msg.into());
                            ui.set_toast_type("error".into());
                            ui.set_toast_action_text("".into());
                            ui.set_toast_visible(true);
                        }
                    });
                    return;
                }
            };
            match resolve_adult_hub_stream_target(&client, &descriptor, Some(&target_variant_id)) {
                Ok((url, method, req_headers)) => {
                    quality_headers = route_playback_headers(&headers, &req_headers);
                    Ok((url, method))
                }
                Err(e) => Err(e),
            }
        };

        match resolution_result {
            Ok((new_stream_url, _play_method)) => {
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak_ui.upgrade() else { return };
                    if !ctx_clone.session_ctrl.is_request_current(quality_req_id) {
                        return;
                    }

                    // Fresh position right before load
                    let target_pos = player_clone.position().unwrap_or(current_pos_secs);
                    *ctx_clone
                        .pending_resume_position
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = if target_pos > 0.0 {
                        Some(target_pos)
                    } else {
                        None
                    };

                    let path = resolve_media_input(&new_stream_url);
                    if let Err(e) =
                        player_clone.load_with_start(&path, &quality_headers, Some(target_pos))
                    {
                        crate::diagnostics::GLOBAL_LOG
                            .e("Player", &format!("Quality load failed: {e}"));
                        // Attempt fallback to previous URL to preserve working source
                        let _ = player_clone.load_with_start(
                            &resolve_media_input(&previous_stream_url),
                            &quality_headers,
                            Some(target_pos),
                        );
                        let _ = player_clone.set_pause(was_paused);
                        ui.set_is_changing_quality(false);
                        ui.set_toast_message(
                            "Failed to switch quality, keeping current stream".into(),
                        );
                        ui.set_toast_type("error".into());
                        ui.set_toast_visible(true);
                        return;
                    }

                    let _ = player_clone.set_speed(current_speed);
                    let _ = player_clone.set_volume(current_volume);
                    let _ = player_clone.set_muted(was_muted);
                    let _ = player_clone.set_pause(was_paused);
                    ui.set_playing(!was_paused);
                    ui.set_url_text(new_stream_url.into());

                    *ctx_clone
                        .active_quality_id
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        target_variant_id.clone();
                    ui.set_player_selected_quality_id(target_variant_id.clone().into());

                    let variants = ctx_clone
                        .current_video_variants
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let opts = format_variant_options(&variants, &target_variant_id);
                    let active_label = opts
                        .iter()
                        .find(|o| o.id == target_variant_id)
                        .map(|o| o.label.to_string())
                        .unwrap_or_else(|| target_variant_id.clone());

                    ui.set_player_selected_quality_label(active_label.clone().into());
                    player_clone.set_quality_variant(Some(active_label));
                    update_slint_quality_options(&ui, &opts);
                });
            }
            Err(err) => {
                crate::diagnostics::GLOBAL_LOG
                    .w("Player", &format!("Quality resolution error: {err}"));
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak_ui.upgrade() else { return };
                    if !ctx_clone.session_ctrl.is_request_current(quality_req_id) {
                        return;
                    }
                    ui.set_is_changing_quality(false);
                    ui.set_toast_message(format!("Could not switch quality: {err}").into());
                    ui.set_toast_type("error".into());
                    ui.set_toast_visible(true);
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use adult_hub_client::VideoVariantDto;

    #[test]
    fn format_variant_options_distinct_resolutions() {
        let variants = vec![
            VideoVariantDto {
                id: "original".into(),
                is_original: true,
                width: Some(3840),
                height: Some(2160),
                codec: Some("hevc".into()),
                mime_type: Some("video/mp4".into()),
                file_size: Some(4_000_000_000),
                duration: Some(100.0),
                bitrate: None,
                estimated_average_bitrate: Some(320_000_000),
            },
            VideoVariantDto {
                id: "alt_1080".into(),
                is_original: false,
                width: Some(1920),
                height: Some(1080),
                codec: Some("h264".into()),
                mime_type: Some("video/mp4".into()),
                file_size: Some(1_000_000_000),
                duration: Some(100.0),
                bitrate: None,
                estimated_average_bitrate: Some(80_000_000),
            },
            VideoVariantDto {
                id: "alt_720".into(),
                is_original: false,
                width: Some(1280),
                height: Some(720),
                codec: Some("h264".into()),
                mime_type: Some("video/mp4".into()),
                file_size: Some(500_000_000),
                duration: Some(100.0),
                bitrate: None,
                estimated_average_bitrate: Some(40_000_000),
            },
        ];

        let opts = format_variant_options(&variants, "alt_1080");
        assert_eq!(opts.len(), 3);

        // 4K Original
        assert_eq!(opts[0].id, "original");
        assert!(opts[0].label.contains("4K"));
        assert!(opts[0].label.contains("Original"));
        assert!(!opts[0].is_selected);
        assert!(opts[0].is_original);

        // 1080p
        assert_eq!(opts[1].id, "alt_1080");
        assert!(opts[1].label.contains("1080p"));
        assert!(opts[1].is_selected);
        assert!(!opts[1].is_original);

        // 720p
        assert_eq!(opts[2].id, "alt_720");
        assert!(opts[2].label.contains("720p"));
        assert!(!opts[2].is_selected);
    }

    #[test]
    fn format_variant_options_duplicate_resolutions_differentiates_codecs_and_size() {
        let variants = vec![
            VideoVariantDto {
                id: "original".into(),
                is_original: true,
                width: Some(1920),
                height: Some(1080),
                codec: Some("h264".into()),
                mime_type: Some("video/mp4".into()),
                file_size: Some(2_000_000_000),
                duration: Some(100.0),
                bitrate: None,
                estimated_average_bitrate: None,
            },
            VideoVariantDto {
                id: "alt_hevc".into(),
                is_original: false,
                width: Some(1920),
                height: Some(1080),
                codec: Some("hevc".into()),
                mime_type: Some("video/mp4".into()),
                file_size: Some(900_000_000),
                duration: Some(100.0),
                bitrate: None,
                estimated_average_bitrate: None,
            },
        ];

        let opts = format_variant_options(&variants, "original");
        assert_eq!(opts.len(), 2);

        // Original 1080p with duplicate resolution must mention H264 and size
        assert!(opts[0].label.contains("Original"));
        assert!(opts[0].label.contains("H264"));
        assert_eq!(opts[0].codec, "H.264");
        assert!(opts[0].is_selected);

        // Alt 1080p must mention HEVC
        assert!(opts[1].label.contains("HEVC"));
        assert_eq!(opts[1].codec, "HEVC");
        assert!(!opts[1].is_selected);
    }

    #[test]
    fn test_normalize_codec_name() {
        assert_eq!(normalize_codec_name("h264"), "H.264");
        assert_eq!(normalize_codec_name("avc1"), "H.264");
        assert_eq!(normalize_codec_name("hevc"), "HEVC");
        assert_eq!(normalize_codec_name("h265"), "HEVC");
        assert_eq!(normalize_codec_name("av1"), "AV1");
        assert_eq!(normalize_codec_name("av01"), "AV1");
        assert_eq!(normalize_codec_name("vp9"), "VP9");
        assert_eq!(normalize_codec_name(""), "");
    }

    #[test]
    fn test_playback_resolution_and_auth_routing() {
        use adult_hub_client::{
            AdultHubClient, PlayMethod, PlaybackDescriptorDto, PlaybackRenditionDto,
        };

        let client = AdultHubClient::new("http://127.0.0.1:8090")
            .expect("valid client")
            .with_token("test_bearer_token_123");

        let descriptor = PlaybackDescriptorDto {
            video_id: "telegram:-100123456:789:012".into(),
            stream_path: Some("/api/v1/playback/stream/telegram%3A-100123456%3A789%3A012".into()),
            mime_type: Some("video/mp4".into()),
            duration_seconds: Some(120.0),
            file_size_bytes: Some(100_000_000),
            width: Some(1920),
            height: Some(1080),
            renditions: vec![
                PlaybackRenditionDto {
                    id: "1080p".into(),
                    label: "1080p Original".into(),
                    mime_type: Some("video/mp4".into()),
                    file_size_bytes: Some(100_000_000),
                    width: Some(1920),
                    height: Some(1080),
                    is_default: true,
                    stream_path: Some(
                        "/api/v1/playback/stream/telegram%3A-100123456%3A789%3A012?rendition=1080p"
                            .into(),
                    ),
                },
                PlaybackRenditionDto {
                    id: "720p".into(),
                    label: "720p".into(),
                    mime_type: Some("video/mp4".into()),
                    file_size_bytes: Some(50_000_000),
                    width: Some(1280),
                    height: Some(720),
                    is_default: false,
                    stream_path: Some(
                        "/api/v1/playback/stream/telegram%3A-100123456%3A789%3A012?rendition=720p"
                            .into(),
                    ),
                },
            ],
        };

        // 1. In API mode, default stream target resolution uses same-origin and passes Bearer auth
        let (url, play_method, auth_headers) =
            resolve_adult_hub_stream_target(&client, &descriptor, None)
                .expect("resolution succeeds");
        assert_eq!(
            url,
            "http://127.0.0.1:8090/api/v1/playback/stream/telegram%3A-100123456%3A789%3A012"
        );
        assert_eq!(play_method, PlayMethod::DirectPlay);
        assert_eq!(
            auth_headers,
            vec!["Authorization: Bearer test_bearer_token_123"]
        );

        // 2. Selected existing rendition routing
        let (r_url, _, _) = resolve_adult_hub_stream_target(&client, &descriptor, Some("720p"))
            .expect("rendition resolution succeeds");
        assert_eq!(
            r_url,
            "http://127.0.0.1:8090/api/v1/playback/stream/telegram%3A-100123456%3A789%3A012?rendition=720p"
        );

        // 3. Non-original rendition selection: if missing or empty stream path, returns error and never defaults
        let missing_rendition_err =
            resolve_adult_hub_stream_target(&client, &descriptor, Some("nonexistent_quality"))
                .expect_err("must fail when selected non-original rendition is missing");
        assert!(missing_rendition_err.contains("nonexistent_quality"));

        let empty_path_descriptor = PlaybackDescriptorDto {
            video_id: "empty_path_vid".into(),
            stream_path: Some("/api/v1/playback/stream/default".into()),
            mime_type: None,
            duration_seconds: None,
            file_size_bytes: None,
            width: None,
            height: None,
            renditions: vec![PlaybackRenditionDto {
                id: "empty_rendition".into(),
                label: "Empty Rendition".into(),
                mime_type: None,
                file_size_bytes: None,
                width: None,
                height: None,
                is_default: false,
                stream_path: Some("   ".into()),
            }],
        };
        let empty_path_err = resolve_adult_hub_stream_target(
            &client,
            &empty_path_descriptor,
            Some("empty_rendition"),
        )
        .expect_err("must fail when non-original rendition stream path is whitespace");
        assert!(empty_path_err.contains("empty_rendition"));

        // 4. Enforces single-origin client policy (rejects cross-origin stream path specifically)
        let hostile_descriptor = PlaybackDescriptorDto {
            video_id: "hostile".into(),
            stream_path: Some("https://evil.attacker.com/leak/stream.mp4".into()),
            mime_type: None,
            duration_seconds: None,
            file_size_bytes: None,
            width: None,
            height: None,
            renditions: Vec::new(),
        };
        let hostile_err = resolve_adult_hub_stream_target(&client, &hostile_descriptor, None)
            .expect_err("must reject cross-origin stream URL");
        assert!(hostile_err.to_ascii_lowercase().contains("origin mismatch"));

        // 5. Enforces rejection of protocol-relative stream URLs specifically
        let proto_rel_descriptor = PlaybackDescriptorDto {
            video_id: "proto_rel".into(),
            stream_path: Some("//evil.attacker.com/leak/stream.mp4".into()),
            mime_type: None,
            duration_seconds: None,
            file_size_bytes: None,
            width: None,
            height: None,
            renditions: Vec::new(),
        };
        let proto_rel_err = resolve_adult_hub_stream_target(&client, &proto_rel_descriptor, None)
            .expect_err("must reject protocol-relative stream URL");
        assert!(
            proto_rel_err
                .to_ascii_lowercase()
                .contains("protocol-relative")
        );

        // 6. HTTPS default port must not trigger a false origin mismatch:
        // url serializes the origin without :443 while ServiceUrl keeps it.
        let https_client = adult_hub_client::AdultHubClient::new("https://media.example")
            .expect("https base URL")
            .with_token("test_bearer_token_123");
        let default_port_descriptor = PlaybackDescriptorDto {
            video_id: "default-port".into(),
            stream_path: Some("/api/v1/playback/stream/default-port".into()),
            mime_type: None,
            duration_seconds: None,
            file_size_bytes: None,
            width: None,
            height: None,
            renditions: Vec::new(),
        };
        let (default_port_url, _, _) =
            resolve_adult_hub_stream_target(&https_client, &default_port_descriptor, None)
                .expect("default HTTPS port must be accepted");
        assert_eq!(
            default_port_url,
            "https://media.example/api/v1/playback/stream/default-port"
        );

        // 7. Header routing preserves the Authorization header produced by
        // the Adult Hub client.
        let api_headers = route_playback_headers(&[], &auth_headers);
        assert!(api_headers.contains(&"Authorization: Bearer test_bearer_token_123".to_string()));
    }

    #[test]
    fn test_route_playback_headers_filters_base_and_required_in_api_mode() {
        let base_headers = vec![
            "Authorization: Bearer stale_token".to_string(),
            "User-Agent: AdultHub/1.0".to_string(),
        ];
        let required_headers = vec![
            "Authorization: Bearer valid_api_token".to_string(),
            "X-Custom-Header: value".to_string(),
        ];
        let routed = route_playback_headers(&base_headers, &required_headers);
        // Base headers: User-Agent kept, stale Authorization filtered
        assert!(routed.contains(&"User-Agent: AdultHub/1.0".to_string()));
        assert!(!routed.contains(&"Authorization: Bearer stale_token".to_string()));

        // Required headers: Authorization and custom headers kept
        assert!(routed.contains(&"Authorization: Bearer valid_api_token".to_string()));
        assert!(routed.contains(&"X-Custom-Header: value".to_string()));
    }

    #[test]
    fn test_resolve_playback_resume_remote_truth_and_fallback() {
        struct TempDirGuard(std::path::PathBuf);
        impl Drop for TempDirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let temp_dir =
            std::env::temp_dir().join(format!("ah-resume-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create test temp dir");
        let _guard = TempDirGuard(temp_dir.clone());
        let db_file = temp_dir.join("test.db");

        let store = adult_hub_storage::LocalStore::open(&db_file).expect("open local store");
        let session_ctrl = PlaybackSessionController::new(Arc::new(
            adult_hub_storage::LocalStore::open(&db_file).unwrap(),
        ));

        let item = TvItem {
            id: "video-test-123".into(),
            runtime_ticks: Some(1_000_000 * 10_000), // 1000s duration
            ..Default::default()
        };

        // Save local progress at 45 seconds (45_000 ms)
        store
            .save_progress("local", &item.id, 45_000, 1_000_000)
            .expect("save local");

        // 1. API mode with remote progress: remote is the truth (e.g. 120s -> 120_000ms)
        let remote_progress = adult_hub_client::WatchProgressDto {
            video_id: item.id.clone(),
            position_seconds: 120,
            duration_seconds: 1000,
            played_percentage: 12.0,
            is_played: false,
            last_watched_at: "2026-11-22T20:00:00Z".into(),
        };
        let resume_api =
            resolve_playback_resume_ms(Some(&remote_progress), &store, &session_ctrl, &item);
        assert_eq!(resume_api, Some(120_000));

        // 2. API mode with completed remote item: remote truth (is_played: true) returns None even if local has progress
        let remote_completed = adult_hub_client::WatchProgressDto {
            video_id: item.id.clone(),
            position_seconds: 980,
            duration_seconds: 1000,
            played_percentage: 98.0,
            is_played: true,
            last_watched_at: "2026-11-22T20:00:00Z".into(),
        };
        let resume_completed =
            resolve_playback_resume_ms(Some(&remote_completed), &store, &session_ctrl, &item);
        assert_eq!(resume_completed, None);

        // 3. API mode with missing / error remote progress (None): falls back to local progress
        let resume_fallback = resolve_playback_resume_ms(None, &store, &session_ctrl, &item);
        assert_eq!(resume_fallback, Some(45_000));
    }

    #[test]
    fn test_playback_completion_and_state_derivation() {
        // Duration: 100 seconds (100_000 ms)
        let duration_ms = 100_000;

        // Position 50s (50%) -> not completed
        assert!(!PlaybackSessionController::is_playback_completed(
            50_000,
            duration_ms,
            false
        ));

        // Position 89s (89%) -> not completed
        assert!(!PlaybackSessionController::is_playback_completed(
            89_000,
            duration_ms,
            false
        ));

        // Position 90s (90%) -> completed
        assert!(PlaybackSessionController::is_playback_completed(
            90_000,
            duration_ms,
            false
        ));

        // Position 95s (95%) -> completed
        assert!(PlaybackSessionController::is_playback_completed(
            95_000,
            duration_ms,
            false
        ));

        // Position 10s with is_eof = true -> completed
        assert!(PlaybackSessionController::is_playback_completed(
            10_000,
            duration_ms,
            true
        ));
    }
}
