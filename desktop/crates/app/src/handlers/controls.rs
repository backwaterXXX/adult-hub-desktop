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

pub fn install_controls(ui: &MainWindow, ctx: &AppContext) {
    let player = ctx.player.clone();
    let autoplay_active = ctx.autoplay_active.clone();
    let hwdec_active = ctx.hwdec_active.clone();
    let reporter = ctx.reporter.clone();
    let session_ctrl = ctx.session_ctrl.clone();
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
                    ui.set_player_state(if paused { "paused".into() } else { "playing".into() });
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
            if !ui.get_player_media_loaded() || !matches!(direction, -1 | 1) {
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
                width: None,
                height: None,
            };
            crate::playback::start_item_playback_with_resume_ctx(
                &ui,
                &ctx_play,
                custom_item,
                None,
            );
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
                        width: None,
                        height: None,
                    };
                    crate::playback::start_item_playback_with_resume_ctx(
                        &ui,
                        &ctx_file,
                        local_item,
                        None,
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
            let muted = !player.is_muted();
            if let Err(error) = player.set_muted(muted) {
                show_error(&ui, error.to_string());
                return;
            }
            ui.set_muted(muted);
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
        const PROFILES: [BufferProfile; 4] = [
            BufferProfile::Standard,
            BufferProfile::FastStart,
            BufferProfile::Large,
            BufferProfile::LowMemory,
        ];
        let prof_idx = Rc::new(Cell::new(0usize));
        ui.on_cycle_buffer_profile(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let next = (prof_idx.get() + 1) % PROFILES.len();
            prof_idx.set(next);
            let profile = PROFILES[next];
            if let Err(error) = player.set_buffer_profile(profile) {
                show_error(&ui, error.to_string());
            } else {
                ui.set_buffer_profile_label(format!("Buffer: {}", profile.name()).into());
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
        let player = player.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let ctx_shuffle = ctx.clone();
        ui.on_play_random(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };

            // 1. Invalidate previous in-flight requests immediately
            let request_id = session_ctrl.next_request_id();

            // 2. Immediately stop previous playback (audio AND video)
            let _ = player.stop();

            // Checkpoint outgoing item and stop reporter session
            let outgoing = current_item.lock().unwrap().take();
            if let Some(prev) = outgoing.as_ref() {
                let pos = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
                let dur = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
                if pos >= 5000 {
                    session_ctrl.checkpoint_progress(prev, pos, dur, false);
                }
                reporter.stop_session(pos);
            }

            // 3. Immediately transition UI into resolving/loading state on the player screen
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
            ui.set_playback_ended_visible(false);
            ui.set_autoplay_countdown_sec(0);
            if ui.get_active_screen() != "player" {
                ctx_shuffle.push_nav_destination(crate::handlers::browsing::current_nav_destination(&ui));
            }
            ui.set_active_screen("player".into());
            ui.set_status("Selecting a random title…".into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));

            let weak = weak.clone();
            let ctx_worker = ctx_shuffle.clone();
            let current_id = outgoing.as_ref().map(|item| item.id.clone());

            std::thread::spawn(move || {
                let session = ctx_worker.credential_store.get().ok().flatten();
                let chosen_item: Option<TvItem> = if let Some(session) = &session {
                    let parent_id = ctx_worker
                        .local_store
                        .get_setting("selected_library_id")
                        .ok()
                        .flatten();
                    let page_res = ctx_worker.jellyfin.get_items(
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
                                ctx_worker.jellyfin.get_items(
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
                            let filtered = filter_catalog_items(items, &ctx_worker.local_store);
                            ctx_worker.shuffle_engine.select_candidate(&filtered, current_id.as_deref())
                        }
                        Err(err) => {
                            crate::diagnostics::GLOBAL_LOG.w("Shuffle", &format!("Jellyfin random query failed: {err}"));
                            None
                        }
                    }
                } else {
                    None
                };

                let final_item = chosen_item.or_else(|| {
                    let catalog = ctx_worker.cached_catalog.lock().unwrap().clone();
                    let filtered = filter_catalog_items(catalog, &ctx_worker.local_store);
                    ctx_worker.shuffle_engine.select_candidate(&filtered, current_id.as_deref())
                });

                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    // If request_id is stale (e.g. user clicked Shuffle again or navigated back), drop it
                    if !ctx_worker.session_ctrl.is_request_current(request_id) {
                        return;
                    }

                    match final_item {
                        Some(target) => {
                            ui.set_status(format!("Playing {}", target.title).into());
                            crate::playback::start_item_playback_with_resume_ctx(
                                &ui,
                                &ctx_worker,
                                target,
                                Some(false),
                            );
                        }
                        None => {
                            ui.set_player_state("failed".into());
                            ui.set_player_error_message(
                                "No random titles are available in this library".into(),
                            );
                            ui.set_status("No random titles available".into());
                            ui.set_status_color(slint::Color::from_rgb_u8(244, 177, 66));
                        }
                    }
                });
            });
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
                ctx_det.player_paused_for_details.store(was_playing, std::sync::atomic::Ordering::SeqCst);
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
                crate::playback::start_item_playback_with_resume_ctx(&ui, &ctx_start, item, Some(false));
            }
        });
    }
}

pub fn install_fullscreen(ui: &MainWindow) {
    let fullscreen = Rc::new(Cell::new(false));

    {
        let ui_weak = ui.as_weak();
        let fullscreen = fullscreen.clone();
        ui.on_toggle_fullscreen(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let next = !fullscreen.get();
            ui.window().set_fullscreen(next);
            fullscreen.set(next);
            ui.set_is_fullscreen(next);
        });
    }

    {
        let ui_weak = ui.as_weak();
        let fullscreen = fullscreen.clone();
        ui.on_escape_pressed(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            if ui.get_player_volume_flyout_open() {
                ui.set_player_volume_flyout_open(false);
                return;
            }
            if fullscreen.get() {
                ui.window().set_fullscreen(false);
                fullscreen.set(false);
                ui.set_is_fullscreen(false);
            } else {
                ui.invoke_navigate_back();
            }
        });
    }
}
