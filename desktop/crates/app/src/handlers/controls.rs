use crate::MainWindow;
use crate::catalog::{filter_catalog_items, sample_catalog_tv_items};
use crate::context::AppContext;
use crate::playback::{pick_media_file, play_random_catalog_item, show_error, start_item_playback};
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
    let headers = ctx.headers.clone();
    let autoplay_active = ctx.autoplay_active.clone();
    let hwdec_active = ctx.hwdec_active.clone();
    let reporter = ctx.reporter.clone();
    let session_ctrl = ctx.session_ctrl.clone();
    let current_item = ctx.current_playing_item.clone();
    let cached_catalog = ctx.cached_catalog.clone();
    let credential_store = ctx.credential_store.clone();
    let local_store = ctx.local_store.clone();
    let jellyfin = ctx.jellyfin.clone();
    let phash_coord = ctx.phash_coord.clone();
    let seek_repeat_state = Arc::new(Mutex::new(SeekRepeatState::default()));
    let seek_feedback_generation = Arc::new(AtomicU64::new(0));

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let reporter = reporter.clone();
        ui.on_toggle_play(move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_player_media_loaded() {
                return;
            }
            let Some(player) = &player else {
                show_error(&ui, "libmpv is not available");
                return;
            };
            match player.toggle_pause() {
                Ok(paused) => {
                    ui.set_playing(!paused);
                    let pos_ms = (player.position().unwrap_or(0.0) * 1000.0) as u64;
                    reporter.report_progress_now(pos_ms, paused);
                }
                Err(error) => show_error(&ui, error.to_string()),
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_retry_current_playback(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else {
                show_error(&ui, "libmpv is not available");
                return;
            };

            let position = player.position().unwrap_or(0.0).max(0.0);
            match player.retry_current_at(position) {
                Ok(()) => {
                    ui.set_player_error_message(String::new().into());
                    ui.set_status("Retrying playback…".into());
                    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
                }
                Err(error) => show_error(&ui, error.to_string()),
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
        let player = player.clone();
        let headers = headers.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let phash_coord = phash_coord.clone();
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
            start_item_playback(
                &ui,
                player,
                custom_item,
                &headers,
                &credential_store,
                &local_store,
                &jellyfin,
                &reporter,
                &session_ctrl,
                &current_item,
                &phash_coord,
            );
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let headers = headers.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let phash_coord = phash_coord.clone();
        ui.on_load_sample(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let sample_item = sample_catalog_tv_items()[0].clone();
            start_item_playback(
                &ui,
                player,
                sample_item,
                &headers,
                &credential_store,
                &local_store,
                &jellyfin,
                &reporter,
                &session_ctrl,
                &current_item,
                &phash_coord,
            );
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let headers = headers.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let phash_coord = phash_coord.clone();
        ui.on_open_file(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
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
                    start_item_playback(
                        &ui,
                        player,
                        local_item,
                        &headers,
                        &credential_store,
                        &local_store,
                        &jellyfin,
                        &reporter,
                        &session_ctrl,
                        &current_item,
                        &phash_coord,
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
        ui.on_toggle_hud(move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_player_media_loaded() {
                return;
            }
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
        let headers = headers.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let cached_catalog = cached_catalog.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let phash_coord = phash_coord.clone();
        ui.on_play_random(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let session = credential_store.get().ok().flatten();
            let Some(session) = session else {
                play_random_catalog_item(
                    &ui,
                    player,
                    &headers,
                    &credential_store,
                    &local_store,
                    &jellyfin,
                    &reporter,
                    &session_ctrl,
                    &current_item,
                    &cached_catalog,
                    &phash_coord,
                );
                return;
            };

            ui.set_status("Finding a random title in your library…".into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let local_store = local_store.clone();
            let current_id = current_item
                .lock()
                .unwrap()
                .as_ref()
                .map(|item| item.id.clone())
                .unwrap_or_default();
            std::thread::spawn(move || {
                let parent_id = local_store
                    .get_setting("selected_library_id")
                    .ok()
                    .flatten();
                let result = jellyfin
                    .get_items(
                        &session.server_url,
                        &session.user_id,
                        &session.access_token,
                        &GetItemsQuery {
                            parent_id: parent_id.as_deref(),
                            limit: 50,
                            sort_by: "Random",
                            ..Default::default()
                        },
                    )
                    .map(|page| filter_catalog_items(page.items, &local_store));
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    match result {
                        Ok(items) => {
                            let target = items.into_iter().find(|item| item.id != current_id);
                            if let Some(target) = target {
                                ui.set_status(format!("Playing {}", target.title).into());
                                ui.invoke_play_item(target.id.into());
                            } else {
                                ui.set_status(
                                    "No random titles are available in this library".into(),
                                );
                                ui.set_status_color(slint::Color::from_rgb_u8(244, 177, 66));
                            }
                        }
                        Err(error) => {
                            ui.set_status(format!("Could not find a random title: {error}").into());
                            ui.set_status_color(slint::Color::from_rgb_u8(244, 67, 54));
                        }
                    }
                })
                .ok();
            });
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_accept_resume(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let pos = ui.get_resume_prompt_position();
            ui.set_resume_prompt_visible(false);
            let _ = player.seek_absolute(pos as f64);
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_reject_resume(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            ui.set_resume_prompt_visible(false);
            let _ = player.seek_absolute(0.0);
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
            if fullscreen.get() {
                ui.window().set_fullscreen(false);
                fullscreen.set(false);
                ui.set_is_fullscreen(false);
            } else if ui.get_active_screen() != "browse" {
                ui.invoke_switch_screen("browse".into());
            }
        });
    }
}
