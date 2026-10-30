use crate::MainWindow;
use crate::catalog::{filter_catalog_items, sample_catalog_tv_items};
use crate::context::AppContext;
use crate::playback::{pick_media_file, show_error};
use adult_hub_client::{GetItemsQuery, TvItem};
use adult_hub_player::BufferProfile;
use slint::ComponentHandle;
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const ASPECTS: [(Option<&str>, &str); 4] = [
    (None, "Auto"),
    (Some("16:9"), "16:9"),
    (Some("4:3"), "4:3"),
    (Some("2.35:1"), "2.35:1"),
];

#[derive(Default)]
struct SeekRepeatState {
    direction: i32,
    base_step_seconds: i32,
    next_step_seconds: i32,
    last_seek_at: Option<Instant>,
}

pub fn apply_and_persist_buffer_profile(
    ui: &MainWindow,
    player: Option<&adult_hub_player::Player>,
    local_store: &adult_hub_storage::LocalStore,
    profile: BufferProfile,
) -> Result<(), String> {
    if let Some(player) = player {
        if let Err(e) = player.set_buffer_profile(profile) {
            return Err(format!("Failed to apply buffer profile: {e}"));
        }
    }
    if let Err(e) = local_store.set_setting("buffer_profile", profile.id()) {
        crate::diagnostics::GLOBAL_LOG.w(
            "Settings",
            &format!("Could not persist buffer profile: {e}"),
        );
    }
    ui.set_buffer_profile(profile.id().into());
    ui.set_buffer_profile_label(format!("Buffer: {}", profile.name()).into());
    ui.set_buffer_profile_description(profile.description().into());
    crate::diagnostics::GLOBAL_LOG.i(
        "Player",
        &format!(
            "Buffer profile active: {} (max_bytes={}, back_bytes={}, readahead={}s, wait={}s)",
            profile.name(),
            profile.demuxer_max_bytes(),
            profile.demuxer_max_back_bytes(),
            profile.demuxer_readahead_secs(),
            profile.cache_pause_wait()
        ),
    );
    Ok(())
}

pub fn select_random_candidate(ctx: &AppContext, current_id: Option<&str>) -> Option<TvItem> {
    let session = ctx.credential_store.get().ok().flatten();
    let chosen_item: Option<TvItem> = if let Some(session) = &session {
        let parent_id = ctx
            .local_store
            .get_setting("selected_library_id")
            .ok()
            .flatten();
        let page_res = ctx.jellyfin.get_items(
            &session.server_url,
            &session.user_id,
            &session.access_token,
            &GetItemsQuery {
                parent_id: parent_id.as_deref(),
                limit: 50,
                sort_by: "Random",
                ..Default::default()
            },
        );
        match page_res {
            Ok(page) => {
                let total = page.total_record_count;
                let items = if total > 50 && fastrand::bool() {
                    let random_offset = fastrand::usize(0..total.saturating_sub(49));
                    ctx.jellyfin
                        .get_items(
                            &session.server_url,
                            &session.user_id,
                            &session.access_token,
                            &GetItemsQuery {
                                parent_id: parent_id.as_deref(),
                                start_index: random_offset,
                                limit: 50,
                                sort_by: "Random",
                                ..Default::default()
                            },
                        )
                        .map(|p| p.items)
                        .unwrap_or(page.items)
                } else {
                    page.items
                };
                let filtered = filter_catalog_items(items, &ctx.local_store);
                ctx.shuffle_engine.select_candidate(&filtered, current_id)
            }
            Err(err) => {
                crate::diagnostics::GLOBAL_LOG
                    .w("Shuffle", &format!("Jellyfin random query failed: {err}"));
                None
            }
        }
    } else {
        None
    };

    chosen_item.or_else(|| {
        let catalog = ctx.cached_catalog.lock().unwrap().clone();
        let filtered = filter_catalog_items(catalog, &ctx.local_store);
        ctx.shuffle_engine.select_candidate(&filtered, current_id)
    })
}

pub fn trigger_movie_switch(ui: &MainWindow, ctx: &AppContext, is_random: bool) {
    if ui.get_is_switching_movie() {
        crate::log_debug("movie_switch: ignoring duplicate switch request while in progress");
        return;
    }

    // 1. Invalidate previous in-flight requests immediately
    let request_id = ctx.session_ctrl.next_request_id();

    // 2. Checkpoint outgoing item's progress BEFORE touching playback state
    let pos = ctx
        .player
        .as_ref()
        .and_then(|p| p.position())
        .unwrap_or(0.0)
        .max(0.0);
    let dur = ctx
        .player
        .as_ref()
        .and_then(|p| p.duration())
        .unwrap_or(0.0)
        .max(0.0);
    let pos_ms = (pos * 1000.0) as u64;
    let dur_ms = (dur * 1000.0) as u64;

    let outgoing = ctx.current_playing_item.lock().unwrap().take();
    if let Some(prev) = outgoing.as_ref() {
        if pos_ms >= 5000 {
            ctx.session_ctrl
                .checkpoint_progress(prev, pos_ms, dur_ms, false);
        }
        ctx.reporter.stop_session(pos_ms);
    }

    // 3. Pause active audio/video without stopping mpv pipeline or discarding buffer in background
    if let Some(player) = &ctx.player {
        let _ = player.set_pause(true);
    }

    // 3. Immediately transition UI into resolving/loading state on the player screen
    ui.set_is_switching_movie(true);
    ui.set_movie_switch_message("Loading next movie…".into());
    ui.set_has_active_media(true);
    ui.set_playing(true);
    ui.set_player_media_loaded(false);
    ui.set_player_state("resolving".into());
    ui.set_player_loading_message("Selecting next video…".into());
    ui.set_player_title("".into());
    ui.set_player_subtitle("".into());
    ui.set_player_error_message(String::new().into());
    ui.set_toast_visible(false);
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
    *ctx.active_quality_id.lock().unwrap() = "original".to_string();
    *ctx.current_video_variants.lock().unwrap() = Vec::new();
    if let Some(player) = &ctx.player {
        player.set_quality_variant(None);
    }
    ui.set_playback_ended_visible(false);
    ui.set_autoplay_countdown_sec(0);
    if ui.get_active_screen() != "player" {
        ctx.push_nav_destination(crate::handlers::browsing::current_nav_destination(ui, ctx));
    }
    ui.set_active_screen("player".into());
    ui.set_status(if is_random {
        "Selecting a random title…".into()
    } else {
        "Loading next movie…".into()
    });
    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));

    let weak = ui.as_weak();
    let ctx_worker = ctx.clone();
    let current_id = outgoing.as_ref().map(|item| item.id.clone());

    std::thread::spawn(move || {
        let final_item = if is_random {
            select_random_candidate(&ctx_worker, current_id.as_deref())
        } else {
            let next_opt = ctx_worker.get_next_contextual_item(current_id.as_deref());
            if next_opt.is_some() {
                next_opt
            } else {
                select_random_candidate(&ctx_worker, current_id.as_deref())
            }
        };

        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            // If request_id is stale (e.g. user navigated back or initiated another switch), drop it
            if !ctx_worker.session_ctrl.is_request_current(request_id) {
                return;
            }

            match final_item {
                Some(target) => {
                    if ctx_worker
                        .active_playback_context
                        .lock()
                        .unwrap()
                        .is_empty()
                    {
                        ctx_worker.set_playback_context(
                            ctx_worker.cached_catalog.lock().unwrap().clone(),
                        );
                    }
                    ui.set_status(format!("Playing {}", target.title).into());
                    crate::playback::start_item_playback_with_resume_ctx(
                        &ui,
                        &ctx_worker,
                        target,
                        Some(false),
                    );
                }
                None => {
                    ui.set_is_switching_movie(false);
                    ui.set_player_state("failed".into());
                    let msg = if is_random {
                        "No random titles are available in this library"
                    } else {
                        "No next titles are available"
                    };
                    ui.set_player_error_message(msg.into());
                    ui.set_status(msg.into());
                    ui.set_status_color(slint::Color::from_rgb_u8(244, 177, 66));
                }
            }
        });
    });
}

pub fn install_controls(ui: &MainWindow, ctx: &AppContext) {
    let player = ctx.player.clone();
    let autoplay_active = ctx.autoplay_active.clone();
    let hwdec_active = ctx.hwdec_active.clone();
    let reporter = ctx.reporter.clone();
    let current_item = ctx.current_playing_item.clone();
    let local_store = ctx.local_store.clone();
    let seek_repeat_state = Arc::new(Mutex::new(SeekRepeatState::default()));
    let seek_feedback_generation = Arc::new(AtomicU64::new(0));

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let reporter = reporter.clone();
        ui.on_toggle_play(move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_player_media_loaded() {
                if ui.get_player_state() == "loading" || ui.get_player_state() == "resolving" {
                    ui.set_player_state("paused".into());
                    ui.set_playing(false);
                } else if ui.get_player_state() == "paused" {
                    ui.set_player_state("loading".into());
                    ui.set_playing(true);
                }
                return;
            }
            let Some(player) = &player else {
                show_error(&ui, "libmpv is not available");
                return;
            };
            match player.toggle_pause() {
                Ok(paused) => {
                    ui.set_playing(!paused);
                    ui.set_player_state(if paused {
                        "paused".into()
                    } else {
                        "playing".into()
                    });
                    let pos_ms = (player.position().unwrap_or(0.0) * 1000.0) as u64;
                    reporter.report_progress_now(pos_ms, paused);
                }
                Err(error) => show_error(&ui, error.to_string()),
            }
        });
    }

    {
        let weak = ui.as_weak();
        let ctx_retry = ctx.clone();
        ui.on_retry_current_playback(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_player_error_message(String::new().into());
            let current_item = ctx_retry.current_playing_item.lock().unwrap().clone();
            if let Some(item) = current_item {
                crate::playback::start_item_playback_with_resume_ctx(&ui, &ctx_retry, item, None);
            } else if let Some(player) = &ctx_retry.player {
                let position = player.position().unwrap_or(0.0).max(0.0);
                match player.retry_current_at(position) {
                    Ok(()) => {
                        ui.set_status("Retrying playback…".into());
                        ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
                    }
                    Err(error) => show_error(&ui, error.to_string()),
                }
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let reporter = reporter.clone();
        ui.on_seek_to(move |seconds| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_player_media_loaded() {
                return;
            }
            let Some(player) = &player else { return };
            if let Err(error) = player.seek_absolute(seconds as f64) {
                show_error(&ui, error.to_string());
            } else {
                let pos_ms = (seconds as f64 * 1000.0) as u64;
                reporter.report_progress_now(pos_ms, player.is_paused());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let reporter = reporter.clone();
        ui.on_seek_relative(move |seconds| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_player_media_loaded() {
                return;
            }
            let Some(player) = &player else { return };
            if let Err(error) = player.seek_relative(seconds as f64) {
                show_error(&ui, error.to_string());
            } else {
                let pos_ms = (player.position().unwrap_or(0.0) * 1000.0) as u64;
                reporter.report_progress_now(pos_ms, player.is_paused());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let reporter = reporter.clone();
        let seek_repeat_state = seek_repeat_state.clone();
        let seek_feedback_generation = seek_feedback_generation.clone();
        ui.on_seek_with_acceleration(move |direction| {
            let Some(ui) = weak.upgrade() else { return };
            if (!ui.get_player_media_loaded() && !ui.get_has_active_media())
                || !matches!(direction, -1 | 1)
            {
                return;
            }
            let Some(player) = &player else { return };
            let base_step = ui.get_seek_step_sec().max(1);
            let now = Instant::now();
            let step = {
                let Ok(mut state) = seek_repeat_state.lock() else {
                    return;
                };
                let is_repeat = state.direction == direction
                    && state.base_step_seconds == base_step
                    && state
                        .last_seek_at
                        .is_some_and(|last| now.duration_since(last) <= Duration::from_secs(1));
                let step = if is_repeat {
                    state.next_step_seconds.max(base_step)
                } else {
                    base_step
                };
                state.direction = direction;
                state.base_step_seconds = base_step;
                state.next_step_seconds = (step + base_step).min(60);
                state.last_seek_at = Some(now);
                step
            };
            let delta_seconds = direction * step;
            if let Err(error) = player.seek_relative(delta_seconds as f64) {
                show_error(&ui, error.to_string());
                return;
            }
            let pos_ms = (player.position().unwrap_or(0.0) * 1000.0) as u64;
            reporter.report_progress_now(pos_ms, player.is_paused());

            let feedback = format!("{delta_seconds:+}s");
            ui.set_seek_feedback(feedback.into());
            let feedback_id = seek_feedback_generation.fetch_add(1, Ordering::SeqCst) + 1;
            let feedback_generation = seek_feedback_generation.clone();
            let weak = weak.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                if feedback_generation.load(Ordering::SeqCst) != feedback_id {
                    return;
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if feedback_generation.load(Ordering::SeqCst) == feedback_id
                        && let Some(ui) = weak.upgrade()
                    {
                        ui.set_seek_feedback("".into());
                    }
                });
            });
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_set_volume(move |volume| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let volume = volume.clamp(0.0, 100.0);
            if let Err(error) = player.set_volume(volume as f64) {
                show_error(&ui, error.to_string());
            }
            ui.set_volume(volume);
            if volume > 0.0 && (ui.get_muted() || player.is_muted()) {
                let _ = player.set_muted(false);
                ui.set_muted(false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let ctx_play = ctx.clone();
        ui.on_load_url(move |text| {
            let Some(ui) = weak.upgrade() else { return };
            let text = text.to_string();
            if text.trim().is_empty() {
                show_error(&ui, "Enter a media URL or file path first");
                return;
            }
            let item_id = if text.starts_with("telegram:") {
                text.clone()
            } else {
                format!("direct:{text}")
            };
            let custom_item = TvItem {
                id: item_id,
                title: Path::new(&text)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Media".into()),
                overview: None,
                year: None,
                runtime_ticks: None,
                genres: vec![],
                tags: vec![],
                studios: vec![],
                people: vec![],
                primary_image_tag: None,
                backdrop_image_tag: None,
                thumb_image_tag: None,
                child_count: None,
                is_favorite: false,
                playback_position_ticks: 0,
                played_percentage: None,
                is_played: false,
                provider_video_id: if text.starts_with("telegram:") {
                    Some(text.replace("telegram:", ""))
                } else {
                    None
                },
                source_provider: text.starts_with("telegram:").then(|| "telegram".into()),
                media_source_id: None,
                container: None,
                file_size_bytes: None,
                is_placeholder_source: false,
                width: None,
                height: None,
                community_rating: None,
            };
            crate::playback::start_item_playback_with_resume_ctx(&ui, &ctx_play, custom_item, None);
        });
    }

    {
        let weak = ui.as_weak();
        let ctx_sample = ctx.clone();
        ui.on_load_sample(move || {
            let Some(ui) = weak.upgrade() else { return };
            let sample_item = sample_catalog_tv_items()[0].clone();
            crate::playback::start_item_playback_with_resume_ctx(
                &ui,
                &ctx_sample,
                sample_item,
                None,
            );
        });
    }

    {
        let weak = ui.as_weak();
        let ctx_file = ctx.clone();
        ui.on_open_file(move || {
            let Some(ui) = weak.upgrade() else { return };
            match pick_media_file() {
                Some(path) => {
                    let local_item = TvItem {
                        id: format!("direct:{path}"),
                        title: Path::new(&path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "Local Video".into()),
                        overview: None,
                        year: None,
                        runtime_ticks: None,
                        genres: vec![],
                        tags: vec![],
                        studios: vec![],
                        people: vec![],
                        primary_image_tag: None,
                        backdrop_image_tag: None,
                        thumb_image_tag: None,
                        child_count: None,
                        is_favorite: false,
                        playback_position_ticks: 0,
                        played_percentage: None,
                        is_played: false,
                        provider_video_id: None,
                        source_provider: None,
                        media_source_id: None,
                        container: None,
                        file_size_bytes: None,
                        is_placeholder_source: false,
                        width: None,
                        height: None,
                        community_rating: None,
                    };
                    crate::playback::start_item_playback_with_resume_ctx(
                        &ui, &ctx_file, local_item, None,
                    );
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
        let player = player.clone();
        ui.on_toggle_hud(move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_player_media_loaded() {
                return;
            }
            let visible = !ui.get_hud_visible();
            ui.set_hud_visible(visible);
            if visible {
                if let Some(player) = &player {
                    ui.set_hud_text(crate::playback::stats_text(&player.stats()).into());
                } else {
                    ui.set_hud_text("Playback statistics unavailable".into());
                }
            } else {
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
            let was_muted = ui.get_muted() || player.is_muted();
            let new_muted = !was_muted;
            if new_muted {
                let _ = player.set_muted(true);
                ui.set_muted(true);
            } else {
                let _ = player.set_muted(false);
                ui.set_muted(false);
                if ui.get_volume() <= 0.0 {
                    ui.set_volume(50.0);
                    let _ = player.set_volume(50.0);
                }
            }
        });
    }

    // Stage 3 Playback Controls
    {
        let weak = ui.as_weak();
        let player = player.clone();
        const SPEEDS: [f64; 4] = [1.0, 1.25, 1.5, 2.0];
        ui.on_cycle_speed(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            // Read the current value from the UI so playback-start resets stay
            // synchronized with this control across consecutive videos.
            let current_speed = ui
                .get_speed_label()
                .trim_end_matches('x')
                .parse::<f64>()
                .unwrap_or(1.0);
            let current_index = SPEEDS
                .iter()
                .position(|speed| (*speed - current_speed).abs() < 0.001)
                .unwrap_or(0);
            let next = (current_index + 1) % SPEEDS.len();
            let speed = SPEEDS[next];
            if let Err(error) = player.set_speed(speed) {
                show_error(&ui, error.to_string());
            } else {
                ui.set_speed_label(format!("{speed:.2}x").into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let local_store = local_store.clone();
        ui.on_select_buffer_profile(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(profile) = BufferProfile::from_id(&id) else {
                show_error(&ui, format!("Unknown buffer profile: {id}"));
                return;
            };
            if let Err(err) =
                apply_and_persist_buffer_profile(&ui, player.as_deref(), &local_store, profile)
            {
                show_error(&ui, err);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let local_store = local_store.clone();
        ui.on_cycle_buffer_profile(move || {
            let Some(ui) = weak.upgrade() else { return };
            let current = player
                .as_ref()
                .map(|p| p.buffer_profile())
                .unwrap_or(BufferProfile::Standard);
            let current_idx = BufferProfile::ALL
                .iter()
                .position(|p| *p == current)
                .unwrap_or(0);
            let next_profile = BufferProfile::ALL[(current_idx + 1) % BufferProfile::ALL.len()];
            if let Err(err) =
                apply_and_persist_buffer_profile(&ui, player.as_deref(), &local_store, next_profile)
            {
                show_error(&ui, err);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let hwdec_active = hwdec_active.clone();
        ui.on_toggle_hwdec(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let active = !hwdec_active.load(Ordering::SeqCst);
            hwdec_active.store(active, Ordering::SeqCst);
            let mode = if active { "auto-safe" } else { "no" };
            if let Err(error) = player.set_hwdec(mode) {
                show_error(&ui, error.to_string());
            } else {
                ui.set_hwdec_active(active);
                ui.set_status(format!("Decoder mode: {mode}").into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let autoplay_active = autoplay_active.clone();
        let local_store = local_store.clone();
        ui.on_toggle_autoplay(move || {
            let Some(ui) = weak.upgrade() else { return };
            let active = !autoplay_active.load(Ordering::SeqCst);
            if let Err(error) =
                local_store.set_setting("autoplay_next", if active { "true" } else { "false" })
            {
                show_error(&ui, format!("Could not save autoplay setting: {error}"));
                return;
            }
            autoplay_active.store(active, Ordering::SeqCst);
            ui.set_autoplay_active(active);
            ui.set_status(if active {
                "Autoplay next enabled".into()
            } else {
                "Autoplay next disabled".into()
            });
        });
    }

    {
        let weak = ui.as_weak();
        let ctx_clone = ctx.clone();
        ui.on_play_random(move || {
            let Some(ui) = weak.upgrade() else { return };
            trigger_movie_switch(&ui, &ctx_clone, true);
        });
    }

    {
        let weak = ui.as_weak();
        let ctx_clone = ctx.clone();
        ui.on_play_next(move || {
            let Some(ui) = weak.upgrade() else { return };
            trigger_movie_switch(&ui, &ctx_clone, false);
        });
    }

    {
        let weak = ui.as_weak();
        let current_item = current_item.clone();
        let player = player.clone();
        let ctx_det = ctx.clone();
        ui.on_open_current_item_details(move || {
            let Some(ui) = weak.upgrade() else { return };
            let maybe_item = current_item.lock().unwrap().clone();
            if let Some(item) = maybe_item {
                let was_playing = ui.get_playing();
                ctx_det
                    .player_paused_for_details
                    .store(was_playing, std::sync::atomic::Ordering::SeqCst);
                if was_playing {
                    if let Some(p) = &player {
                        let _ = p.set_pause(true);
                    }
                    ui.set_playing(false);
                }
                ui.invoke_open_item_detail(item.id.into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let current_item = current_item.clone();
        ui.on_replay_current(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_playback_ended_visible(false);
            ui.set_autoplay_countdown_sec(0);
            if let Some(player) = &player {
                if ui.get_player_media_loaded() {
                    let _ = player.seek_absolute(0.0);
                    let _ = player.set_pause(false);
                    ui.set_playing(true);
                    return;
                }
            }
            let maybe_item = current_item.lock().unwrap().clone();
            if let Some(item) = maybe_item {
                ui.invoke_play_item(item.id.into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let current_item = current_item.clone();
        let ctx_start = ctx.clone();
        ui.on_play_current_from_start(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_playback_ended_visible(false);
            ui.set_autoplay_countdown_sec(0);
            if let Some(player) = &player {
                if ui.get_player_media_loaded() {
                    let _ = player.seek_absolute(0.0);
                    let _ = player.set_pause(false);
                    ui.set_player_state("playing".into());
                    ui.set_playing(true);
                    return;
                }
            }
            let maybe_item = current_item.lock().unwrap().clone();
            if let Some(item) = maybe_item {
                crate::playback::start_item_playback_with_resume_ctx(
                    &ui,
                    &ctx_start,
                    item,
                    Some(false),
                );
            }
        });
    }

    {
        let weak = ui.as_weak();
        let ctx_quality = ctx.clone();
        ui.on_select_quality(move |variant_id| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_player_quality_panel_open(false);
            ui.set_player_quality_subpage_open(false);
            crate::playback::switch_video_quality(&ui, &ctx_quality, variant_id.to_string());
        });
    }

    // Download handlers
    {
        let ui_weak = ui.as_weak();
        let ctx_dl = ctx.clone();
        ui.on_trigger_download(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let current_item = ctx_dl.current_playing_item.lock().unwrap().clone();
            let Some(item) = current_item else {
                ui.set_toast_message("No active video to download".into());
                ui.set_toast_type("info".into());
                ui.set_toast_action_text("".into());
                ui.set_toast_visible(true);
                return;
            };

            if ctx_dl
                .download_manager
                .is_active_or_queued_for_item(&item.id)
            {
                ui.set_download_panel_open(!ui.get_download_panel_open());
                return;
            }

            let ext =
                crate::download::normalize_container_extension(item.container.as_deref(), None);
            let suggested_name = crate::download::build_suggested_filename(&item.title, ext);

            if let Some(target_path) =
                crate::download::prompt_save_file_destination(&suggested_name, ext)
            {
                // Download queues or starts in the background without automatically opening the panel.
                if !ctx_dl.download_manager.is_downloading() {
                    ui.set_download_status("preparing".into());
                    ui.set_download_title(item.title.clone().into());
                    ui.set_download_item_id(item.id.clone().into());
                    ui.set_download_indeterminate(true);
                    ui.set_download_progress(0.0);
                    ui.set_download_bytes_text("Preparing video stream…".into());
                    ui.set_download_speed_text("".into());
                }
                crate::download::start_item_download(
                    &ctx_dl,
                    &item,
                    target_path,
                    ctx_dl.download_manager.clone(),
                );
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_toggle_download_panel(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            ui.set_download_panel_open(!ui.get_download_panel_open());
            ui.set_player_controls_visible(true);
        });
    }

    {
        let ui_weak = ui.as_weak();
        let ctx_dl = ctx.clone();
        ui.on_cancel_download(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            ctx_dl.download_manager.cancel();
            ui.set_download_status("error".into());
            ui.set_download_error_text("Download cancelled".into());
        });
    }

    {
        let ctx_dl = ctx.clone();
        ui.on_pause_download(move || {
            ctx_dl.download_manager.pause();
        });
    }

    {
        let ctx_dl = ctx.clone();
        ui.on_resume_download(move || {
            ctx_dl.download_manager.resume(&ctx_dl);
        });
    }

    {
        let ctx_dl = ctx.clone();
        ui.on_cancel_queued_download(move |id| {
            ctx_dl.download_manager.cancel_queued(&id);
        });
    }

    {
        ui.on_open_download_file(move |path| {
            crate::download::open_file(std::path::Path::new(path.as_str()));
        });
    }

    {
        ui.on_open_download_folder_for_path(move |path| {
            crate::download::open_folder_in_explorer(std::path::Path::new(path.as_str()));
        });
    }

    {
        let ui_weak = ui.as_weak();
        let ctx_dl = ctx.clone();
        ui.on_retry_download(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let task = ctx_dl.download_manager.current_task();
            let Some(task) = task else { return };
            let item = ctx_dl
                .find_catalog_item(&task.item_id)
                .or_else(|| ctx_dl.current_playing_item.lock().unwrap().clone());
            let Some(item) = item else { return };

            ui.set_download_panel_open(true);
            ui.set_download_status("preparing".into());
            ui.set_download_title(item.title.clone().into());
            ui.set_download_item_id(item.id.clone().into());
            ui.set_download_indeterminate(true);
            ui.set_download_progress(0.0);
            ui.set_download_bytes_text("Preparing download…".into());
            crate::download::start_item_download(
                &ctx_dl,
                &item,
                task.target_path.clone(),
                ctx_dl.download_manager.clone(),
            );
        });
    }

    {
        let ui_weak = ui.as_weak();
        let ctx_dl = ctx.clone();
        ui.on_open_download_folder(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            if let Some(task) = ctx_dl.download_manager.current_task() {
                if let Ok(st) = task.status.lock() {
                    if let crate::download::DownloadStatus::Completed {
                        ref folder_path, ..
                    } = *st
                    {
                        crate::download::open_folder_in_explorer(folder_path);
                        return;
                    }
                }
                crate::download::open_folder_in_explorer(&task.target_path);
            } else {
                let file_path = ui.get_download_file_path();
                if !file_path.is_empty() {
                    crate::download::open_folder_in_explorer(std::path::Path::new(
                        file_path.as_str(),
                    ));
                }
            }
        });
    }
}

use slint::winit_030::{WinitWindowAccessor, winit};

pub fn install_fullscreen(ui: &MainWindow) {
    let fullscreen = Rc::new(Cell::new(false));
    let was_maximized = Rc::new(Cell::new(false));
    let saved_normal_size = Rc::new(Cell::new((1180.0_f32, 760.0_f32)));
    let saved_normal_phys = Rc::new(Cell::new(Option::<(u32, u32)>::None));
    let saved_normal_pos = Rc::new(Cell::new(Option::<(i32, i32)>::None));
    #[cfg(target_os = "windows")]
    let saved_win32_placement = Rc::new(Cell::new(
        Option::<windows_sys::Win32::UI::WindowsAndMessaging::WINDOWPLACEMENT>::None,
    ));

    let toggle_fullscreen = {
        let ui_weak = ui.as_weak();
        let fullscreen = fullscreen.clone();
        let was_maximized = was_maximized.clone();
        let saved_normal_size = saved_normal_size.clone();
        let saved_normal_phys = saved_normal_phys.clone();
        let saved_normal_pos = saved_normal_pos.clone();
        #[cfg(target_os = "windows")]
        let saved_win32_placement = saved_win32_placement.clone();

        Rc::new(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let is_currently_fs =
                fullscreen.get() || ui.get_is_fullscreen() || ui.window().is_fullscreen();
            let next_fs = !is_currently_fs;

            if next_fs {
                // Entering fullscreen
                let is_max = ui.window().is_maximized();
                was_maximized.set(is_max);

                ui.window().with_winit_window(|window| {
                    let inner = window.inner_size();
                    let scale = window.scale_factor();
                    let (logical_w, logical_h) = if scale > 0.0 {
                        ((inner.width as f64 / scale) as f32, (inner.height as f64 / scale) as f32)
                    } else {
                        (ui.get_window_target_width(), ui.get_window_target_height())
                    };
                    saved_normal_size.set((logical_w, logical_h));
                    saved_normal_phys.set(Some((inner.width, inner.height)));
                    saved_normal_pos.set(window.outer_position().ok().map(|p| (p.x, p.y)));

                    #[cfg(target_os = "windows")]
                    {
                        use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowPlacement, WINDOWPLACEMENT};
                        use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

                        if let Ok(handle) = window.window_handle() {
                            if let RawWindowHandle::Win32(win32_handle) = handle.as_raw() {
                                let hwnd = win32_handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                                let mut wp: WINDOWPLACEMENT = unsafe { std::mem::zeroed() };
                                wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
                                if unsafe { GetWindowPlacement(hwnd, &mut wp) } != 0 {
                                    saved_win32_placement.set(Some(wp));
                                }
                            }
                        }
                    }

                    // Query monitor dimensions and update window-target-width/height so Slint's preferred layout expands
                    if let Some(mon) = window.current_monitor() {
                        let msize = mon.size();
                        let (mw, mh) = if scale > 0.0 {
                            ((msize.width as f64 / scale) as f32, (msize.height as f64 / scale) as f32)
                        } else {
                            (msize.width as f32, msize.height as f32)
                        };
                        ui.set_window_target_width(mw);
                        ui.set_window_target_height(mh);
                    }

                    crate::log_debug(&format!(
                        "fullscreen: entering fullscreen (was_maximized: {is_max}, normal target size: {logical_w:.0}x{logical_h:.0})"
                    ));

                    fullscreen.set(true);
                    ui.set_is_fullscreen(true);
                    ui.window().set_fullscreen(true);
                    window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));

                    #[cfg(target_os = "windows")]
                    {
                        use windows_sys::Win32::Graphics::Gdi::{
                            GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
                        };
                        use windows_sys::Win32::UI::WindowsAndMessaging::{
                            GetWindowLongW, SetWindowLongW, SetWindowPos, GWL_STYLE,
                            HWND_TOP, SWP_FRAMECHANGED, SWP_NOOWNERZORDER, SWP_SHOWWINDOW,
                            WS_MAXIMIZE, WS_OVERLAPPEDWINDOW, WS_POPUP,
                        };
                        use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

                        if let Ok(handle) = window.window_handle() {
                            if let RawWindowHandle::Win32(win32_handle) = handle.as_raw() {
                                let hwnd = win32_handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                                let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
                                let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
                                mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                                if unsafe { GetMonitorInfoW(hmon, &mut mi) } != 0 {
                                    let x = mi.rcMonitor.left;
                                    let y = mi.rcMonitor.top;
                                    let w = mi.rcMonitor.right - mi.rcMonitor.left;
                                    let h = mi.rcMonitor.bottom - mi.rcMonitor.top;

                                    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
                                    unsafe {
                                        SetWindowLongW(
                                            hwnd,
                                            GWL_STYLE,
                                            ((style & !(WS_OVERLAPPEDWINDOW | WS_MAXIMIZE)) | WS_POPUP) as i32,
                                        );
                                        SetWindowPos(
                                            hwnd,
                                            HWND_TOP,
                                            x,
                                            y,
                                            w,
                                            h,
                                            SWP_FRAMECHANGED | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
                                        );
                                        windows_sys::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
                                        windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(hwnd);
                                    }
                                }
                            }
                        }
                    }

                    window.focus_window();

                    // Diagnostic logging immediately after applying state change
                    let actual_winit = window.fullscreen().is_some();
                    let actual_slint = ui.window().is_fullscreen();
                    let inner = window.inner_size();
                    let outer = window.outer_position().ok();
                    let surface = ui.window().size();
                    let scale = window.scale_factor();
                    let monitor_info = window.current_monitor().map(|m| (m.position(), m.size()));

                    let diag = format_fullscreen_diagnostics(
                        true,
                        actual_winit,
                        actual_slint,
                        inner.width,
                        inner.height,
                        outer.map(|p| (p.x, p.y)),
                        surface.width,
                        surface.height,
                        scale,
                        monitor_info.map(|(p, s)| ((p.x, p.y), (s.width, s.height))),
                    );
                    crate::log_debug(&diag);
                });

                // Immediate frame redraw during pause or playing
                ui.window().request_redraw();
                ui.invoke_focus_keys();
            } else {
                // Exiting fullscreen
                crate::log_debug("fullscreen: exiting fullscreen mode");
                fullscreen.set(false);
                ui.set_is_fullscreen(false);
                ui.window().set_fullscreen(false);

                ui.window().with_winit_window(|window| {
                    window.set_fullscreen(None);

                    #[cfg(target_os = "windows")]
                    {
                        use windows_sys::Win32::UI::WindowsAndMessaging::{
                            GetWindowLongW, SetWindowLongW, SetWindowPlacement, SetWindowPos, GWL_STYLE,
                            SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
                            SWP_SHOWWINDOW,
                            WS_OVERLAPPEDWINDOW, WS_POPUP,
                        };
                        use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

                        if let Ok(handle) = window.window_handle() {
                            if let RawWindowHandle::Win32(win32_handle) = handle.as_raw() {
                                let hwnd = win32_handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                                let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
                                unsafe {
                                    SetWindowLongW(hwnd, GWL_STYLE, ((style & !WS_POPUP) | WS_OVERLAPPEDWINDOW) as i32);
                                }
                                if let Some(mut wp) = saved_win32_placement.take() {
                                    unsafe {
                                        if !was_maximized.get() {
                                            wp.showCmd = windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL as u32;
                                            SetWindowPlacement(hwnd, &wp);
                                            let norm_w = (wp.rcNormalPosition.right - wp.rcNormalPosition.left).max(100);
                                            let norm_h = (wp.rcNormalPosition.bottom - wp.rcNormalPosition.top).max(100);
                                            let norm_x = wp.rcNormalPosition.left;
                                            let norm_y = wp.rcNormalPosition.top;
                                            SetWindowPos(
                                                hwnd,
                                                0 as _,
                                                norm_x,
                                                norm_y,
                                                norm_w,
                                                norm_h,
                                                SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED | SWP_SHOWWINDOW,
                                            );
                                        } else {
                                            wp.showCmd = windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWMAXIMIZED as u32;
                                            SetWindowPlacement(hwnd, &wp);
                                            SetWindowPos(
                                                hwnd,
                                                0 as _,
                                                0,
                                                0,
                                                0,
                                                0,
                                                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
                                            );
                                        }
                                    }
                                }
                                unsafe {
                                    windows_sys::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
                                    windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(hwnd);
                                }
                            }
                        }
                    }

                    window.focus_window();

                    if was_maximized.get() {
                        crate::log_debug("fullscreen: restoring previous maximized state");
                        ui.window().set_maximized(true);
                        window.set_maximized(true);
                    } else {
                        let (w, h) = saved_normal_size.get();
                        crate::log_debug(&format!("fullscreen: restoring previous normal rect: {w:.0}x{h:.0}"));
                        ui.set_window_target_width(w);
                        ui.set_window_target_height(h);
                        if let Some((pw, ph)) = saved_normal_phys.get() {
                            let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(pw, ph));
                        }
                        if let Some((x, y)) = saved_normal_pos.get() {
                            window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
                            ui.window().set_position(slint::WindowPosition::Physical(
                                slint::PhysicalPosition::new(x, y),
                            ));
                        }
                        ui.window().set_maximized(false);
                        window.set_maximized(false);
                    }

                    // Diagnostic logging immediately after exiting
                    let actual_winit = window.fullscreen().is_some();
                    let actual_slint = ui.window().is_fullscreen();
                    let inner = window.inner_size();
                    let outer = window.outer_position().ok();
                    let surface = ui.window().size();
                    let scale = window.scale_factor();
                    let monitor_info = window.current_monitor().map(|m| (m.position(), m.size()));

                    let diag = format_fullscreen_diagnostics(
                        false,
                        actual_winit,
                        actual_slint,
                        inner.width,
                        inner.height,
                        outer.map(|p| (p.x, p.y)),
                        surface.width,
                        surface.height,
                        scale,
                        monitor_info.map(|(p, s)| ((p.x, p.y), (s.width, s.height))),
                    );
                    crate::log_debug(&diag);
                });

                // Immediate frame redraw during pause or playing
                ui.window().request_redraw();
                ui.invoke_focus_keys();
            }
        })
    };

    {
        let toggle = toggle_fullscreen.clone();
        ui.on_toggle_fullscreen(move || {
            toggle();
        });
    }

    {
        let ui_weak = ui.as_weak();
        let fullscreen = fullscreen.clone();
        let toggle = toggle_fullscreen.clone();
        ui.on_escape_pressed(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            if ui.get_player_quality_panel_open() {
                ui.set_player_quality_panel_open(false);
                return;
            }
            if ui.get_player_quality_subpage_open() {
                ui.set_player_quality_subpage_open(false);
                return;
            }
            if ui.get_player_more_controls_visible() {
                ui.set_player_more_controls_visible(false);
                ui.set_player_quality_subpage_open(false);
                return;
            }
            if ui.get_player_volume_flyout_open() {
                ui.set_player_volume_flyout_open(false);
                return;
            }
            if ui.get_download_panel_open() {
                ui.set_download_panel_open(false);
                return;
            }
            if fullscreen.get() || ui.get_is_fullscreen() || ui.window().is_fullscreen() {
                toggle();
            } else {
                ui.invoke_navigate_back();
            }
        });
    }
}

pub fn format_fullscreen_diagnostics(
    requested: bool,
    actual_winit: bool,
    actual_slint: bool,
    inner_w: u32,
    inner_h: u32,
    outer_pos: Option<(i32, i32)>,
    surface_w: u32,
    surface_h: u32,
    scale: f64,
    monitor: Option<((i32, i32), (u32, u32))>,
) -> String {
    format!(
        "fullscreen diagnostics: requested={requested}, actual_winit={actual_winit}, actual_slint={actual_slint}, inner_size=({inner_w},{inner_h}), outer_pos={outer_pos:?}, surface_size=({surface_w},{surface_h}), scale={scale:.2}, monitor={monitor:?}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_diagnostics_formatting_contains_all_required_fields() {
        let text = format_fullscreen_diagnostics(
            true,
            true,
            true,
            1920,
            1080,
            Some((0, 0)),
            1920,
            1080,
            1.25,
            Some(((0, 0), (1920, 1080))),
        );
        assert!(text.contains("requested=true"));
        assert!(text.contains("actual_winit=true"));
        assert!(text.contains("actual_slint=true"));
        assert!(text.contains("inner_size=(1920,1080)"));
        assert!(text.contains("outer_pos=Some((0, 0))"));
        assert!(text.contains("surface_size=(1920,1080)"));
        assert!(text.contains("scale=1.25"));
        assert!(text.contains("monitor=Some(((0, 0), (1920, 1080)))"));
    }

    #[test]
    fn fullscreen_exit_diagnostics_preserves_unfullscreen_values() {
        let text = format_fullscreen_diagnostics(
            false,
            false,
            false,
            1180,
            790,
            Some((100, 100)),
            1180,
            790,
            1.0,
            None,
        );
        assert!(text.contains("requested=false"));
        assert!(text.contains("actual_winit=false"));
        assert!(text.contains("actual_slint=false"));
        assert!(text.contains("inner_size=(1180,790)"));
        assert!(text.contains("outer_pos=Some((100, 100))"));
        assert!(text.contains("scale=1.00"));
        assert!(text.contains("monitor=None"));
    }

    #[test]
    fn test_volume_clamping_and_unmuting_rules() {
        let compute_volume = |vol: f32, muted: bool| -> (f32, bool) {
            let clamped = vol.clamp(0.0, 100.0);
            let new_muted = if clamped > 0.0 && muted { false } else { muted };
            (clamped, new_muted)
        };

        assert_eq!(compute_volume(120.0, true), (100.0, false));
        assert_eq!(compute_volume(-15.0, true), (0.0, true));
        assert_eq!(compute_volume(45.0, true), (45.0, false));
        assert_eq!(compute_volume(0.0, true), (0.0, true));
        assert_eq!(compute_volume(60.0, false), (60.0, false));
    }
}
