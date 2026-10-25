use crate::MainWindow;
use crate::context::AppContext;
use crate::phash::PhashCoordinator;
use crate::reporter::PlaybackReporter;
use crate::session::PlaybackSessionController;
use adult_hub_client::{JellyfinClient, TvItem};
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

    if let Some(item) = current_item.lock().unwrap().take() {
        session_ctrl.checkpoint_progress(
            &item,
            position_ms,
            duration_ms,
            ui.get_playback_ended_visible(),
        );
    }
    reporter.stop_session(position_ms);
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
    let resolver_url =
        crate::context::AppContext::resolve_effective_resolver_url(local_store, credential_store);

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
                        *current_item.lock().unwrap() = Some(item.clone());
                        ui.set_has_active_media(true);
                        ui.set_player_title(crate::catalog::display_title(&item).into());
                        ui.set_player_subtitle(format_player_subtitle(&item).into());
                        ui.set_active_screen("player".into());
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
                    *current_item.lock().unwrap() = Some(item.clone());
                    ui.set_has_active_media(true);
                    ui.set_player_title(crate::catalog::display_title(&item).into());
                    ui.set_player_subtitle(format_player_subtitle(&item).into());
                    ui.set_active_screen("player".into());
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

    let calculated_resume_ms =
        PlaybackSessionController::resolve_item_resume_position(local_store, session_ctrl, &item);

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
    *current_item.lock().unwrap() = Some(item.clone());
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

pub fn start_item_playback_ctx(ui: &MainWindow, ctx: &AppContext, item: TvItem) {
    start_item_playback_with_resume_ctx(ui, ctx, item, None);
}

pub fn start_item_playback_with_resume_ctx(
    ui: &MainWindow,
    ctx: &AppContext,
    item: TvItem,
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
    if let Some(prev) = ctx.current_playing_item.lock().unwrap().take() {
        if pos >= 5000 && prev.id != item.id {
            ctx.session_ctrl.checkpoint_progress(&prev, pos, dur, false);
        }
        ctx.reporter.stop_session(pos);
    }

    // Pause outgoing playback without stopping mpv pipeline or discarding buffer
    let _ = player.set_pause(true);

    // 2. Immediately update UI to show player screen and loading/resolving overlay
    *ctx.current_playing_item.lock().unwrap() = Some(item.clone());
    ui.set_has_active_media(true);
    ui.set_playing(true);
    ui.set_player_is_favorite(item.is_favorite);
    ui.set_player_favorite_enabled(!item.is_telegram() && uuid::Uuid::parse_str(&item.id).is_ok());
    ui.set_player_error_message(String::new().into());
    ui.set_toast_visible(false); // Clear any stale error toast
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
    ui.set_player_state("resolving".into());
    ui.set_player_loading_message("Resolving stream address…".into());
    ui.set_player_title(crate::catalog::display_title(&item).into());
    ui.set_player_subtitle(format_player_subtitle(&item).into());
    ui.set_status(format!("Preparing: {}", item.title).into());
    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
    ui.set_player_is_telegram_stream(item.is_telegram());
    ui.set_player_has_quality_alternatives(false);
    ui.set_player_quality_subpage_open(false);
    ui.set_player_selected_quality_id("original".into());
    ui.set_player_selected_quality_label("Original".into());
    ui.set_player_quality_options(slint::ModelRc::default());
    *ctx.active_quality_id.lock().unwrap() = "original".to_string();
    *ctx.current_video_variants.lock().unwrap() = Vec::new();
    player.set_quality_variant(None);

    if ui.get_active_screen() != "player" {
        ctx.push_nav_destination(crate::handlers::browsing::current_nav_destination(ui, ctx));
    }
    ui.set_active_screen("player".into());

    // 3. Resolve stream URL in background thread
    let weak_ui = ui.as_weak();
    let ctx_clone = ctx.clone();
    let headers = ctx.headers.clone();

    // Asynchronously fetch video quality alternatives for Telegram content
    if item.is_telegram() {
        let weak_ui_variants = ui.as_weak();
        let ctx_variants = ctx.clone();
        let vid_ref = if item.id.starts_with("telegram:") {
            item.id.clone()
        } else if let Some(ref vid) = item.provider_video_id {
            if vid.starts_with("telegram:") {
                vid.clone()
            } else {
                format!("telegram:{vid}")
            }
        } else {
            format!("telegram:{}", item.id)
        };

        std::thread::spawn(move || {
            let resolver_url = ctx_variants.get_resolver_url();
            let Some(base_url) = resolver_url else { return };
            if base_url.trim().is_empty() { return };

            match ctx_variants.resolver.get_video_variants(&base_url, &vid_ref) {
                Ok(response) => {
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(ui) = weak_ui_variants.upgrade() else { return };
                        if !ctx_variants.session_ctrl.is_request_current(request_id) {
                            return;
                        }
                        *ctx_variants.current_video_variants.lock().unwrap() = response.variants.clone();
                        let active_id = ctx_variants.active_quality_id.lock().unwrap().clone();

                        if response.has_alternatives && response.variants.len() > 1 {
                            let opts = format_variant_options(&response.variants, &active_id);
                            let active_label = opts
                                .iter()
                                .find(|o| o.id == active_id)
                                .map(|o| o.label.to_string())
                                .unwrap_or_else(|| "Original".to_string());

                            ui.set_player_has_quality_alternatives(true);
                            ui.set_player_selected_quality_label(active_label.clone().into());
                            if let Some(player) = &ctx_variants.player {
                                player.set_quality_variant(Some(active_label));
                            }
                            update_slint_quality_options(&ui, &opts);
                        } else {
                            ui.set_player_has_quality_alternatives(false);
                            let orig_label = response
                                .variants
                                .iter()
                                .find(|v| v.is_original)
                                .map(|v| v.display_label(false))
                                .unwrap_or_else(|| "Original".to_string());
                            ui.set_player_selected_quality_label(orig_label.clone().into());
                            if let Some(player) = &ctx_variants.player {
                                player.set_quality_variant(Some(orig_label));
                            }
                        }
                    });
                }
                Err(err) => {
                    crate::diagnostics::GLOBAL_LOG.w(
                        "Resolver",
                        &format!("Could not fetch video variants for {vid_ref}: {err}"),
                    );
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(ui) = weak_ui_variants.upgrade() else { return };
                        if !ctx_variants.session_ctrl.is_request_current(request_id) {
                            return;
                        }
                        ui.set_player_has_quality_alternatives(false);
                        ui.set_player_selected_quality_label("Original".into());
                    });
                }
            }
        });
    }

    std::thread::spawn(move || {
        let resolve_started = std::time::Instant::now();
        let session = ctx_clone.credential_store.get().ok().flatten();
        let resolver_url = ctx_clone.get_resolver_url();

        let mut resolved_media_source_id = item.media_source_id.clone();
        let mut play_session_id = None;
        let resolution_result = if item.is_telegram()
            || item.id == "sample"
            || uuid::Uuid::parse_str(&item.id).is_err()
        {
            PlaybackSessionController::resolve_stream_url(
                &item,
                session.as_ref().map(|s| s.server_url.as_str()),
                resolver_url.as_deref(),
                session.as_ref().map(|s| s.access_token.as_str()),
            )
            .map(|(url, method)| (url, method, Vec::new()))
        } else {
            let Some(session) = session.as_ref() else {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak_ui.upgrade() {
                        ui.set_player_state("failed".into());
                        show_error(&ui, "Sign in to Jellyfin before playing this item.");
                    }
                });
                return;
            };
            match ctx_clone.jellyfin.resolve_playback(
                &session.server_url,
                &session.user_id,
                &session.access_token,
                &item.id,
            ) {
                Ok(resolution) => {
                    resolved_media_source_id = resolution.media_source_id;
                    play_session_id = resolution.play_session_id;
                    Ok((
                        resolution.stream_url,
                        resolution.play_method,
                        resolution.required_headers,
                    ))
                }
                Err(err) => Err(format!("Could not resolve Jellyfin playback: {err}")),
            }
        };

        let (stream_url, play_method, required_headers) = match resolution_result {
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

        let mut stream_headers = headers;
        stream_headers.extend(required_headers);
        if uuid::Uuid::parse_str(&item.id).is_ok() {
            if let Some(sess) = &session {
                if !sess.access_token.is_empty() {
                    stream_headers.push(format!("X-Emby-Token: {}", sess.access_token));
                }
            }
        }

        // Determine resume position
        let duration_ms = item.runtime_ticks.map(|t| (t / 10_000) as u64).unwrap_or(0);
        let mut start_pos_secs = 0.0;

        let calculated_resume_ms = PlaybackSessionController::resolve_item_resume_position(
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
                *ctx_clone.pending_resume_position.lock().unwrap() = start_pos_opt;

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

                let is_jellyfin_item = uuid::Uuid::parse_str(&item.id).is_ok();
                if is_jellyfin_item {
                    ctx_clone.reporter.start_session(
                        &item.id,
                        resolved_media_source_id,
                        play_session_id,
                        play_method,
                        (start_pos_secs * 1000.0) as u64,
                    );
                }

                let phash_enabled = ctx_clone
                    .local_store
                    .get_setting("phash_on_playback")
                    .ok()
                    .flatten()
                    .map(|v| v == "true" || v == "1")
                    .unwrap_or(false);
                if phash_enabled && (is_jellyfin_item || item.is_telegram()) {
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
    _jellyfin: &Arc<JellyfinClient>,
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
            let codec_norm = v.codec.as_deref().map(normalize_codec_name).unwrap_or_default();
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

pub fn switch_video_quality(
    ui: &MainWindow,
    ctx: &AppContext,
    target_variant_id: String,
) {
    let Some(player) = ctx.player.clone() else {
        return;
    };
    let current_variant = ctx.active_quality_id.lock().unwrap().clone();
    if current_variant == target_variant_id {
        return;
    }
    if ui.get_is_changing_quality() {
        return;
    }

    let item = ctx.current_playing_item.lock().unwrap().clone();
    let Some(item) = item else {
        return;
    };
    if !item.is_telegram() {
        return;
    }

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
        let resolver_url = ctx_clone.get_resolver_url();

        let resolution_result = PlaybackSessionController::resolve_stream_url_with_variant(
            &item,
            session.as_ref().map(|s| s.server_url.as_str()),
            resolver_url.as_deref(),
            session.as_ref().map(|s| s.access_token.as_str()),
            Some(&target_variant_id),
        );

        match resolution_result {
            Ok((new_stream_url, _play_method)) => {
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak_ui.upgrade() else { return };
                    if !ctx_clone.session_ctrl.is_request_current(quality_req_id) {
                        return;
                    }

                    // Fresh position right before load
                    let target_pos = player_clone.position().unwrap_or(current_pos_secs);
                    *ctx_clone.pending_resume_position.lock().unwrap() = if target_pos > 0.0 {
                        Some(target_pos)
                    } else {
                        None
                    };

                    let path = resolve_media_input(&new_stream_url);
                    if let Err(e) = player_clone.load_with_start(&path, &headers, Some(target_pos)) {
                        crate::diagnostics::GLOBAL_LOG.e("Player", &format!("Quality load failed: {e}"));
                        // Attempt fallback to previous URL to preserve working source
                        let _ = player_clone.load_with_start(
                            &resolve_media_input(&previous_stream_url),
                            &headers,
                            Some(target_pos),
                        );
                        let _ = player_clone.set_pause(was_paused);
                        ui.set_is_changing_quality(false);
                        ui.set_toast_message("Failed to switch quality, keeping current stream".into());
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

                    *ctx_clone.active_quality_id.lock().unwrap() = target_variant_id.clone();
                    ui.set_player_selected_quality_id(target_variant_id.clone().into());

                    let variants = ctx_clone.current_video_variants.lock().unwrap().clone();
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
                crate::diagnostics::GLOBAL_LOG.w("Player", &format!("Quality resolution error: {err}"));
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
}
