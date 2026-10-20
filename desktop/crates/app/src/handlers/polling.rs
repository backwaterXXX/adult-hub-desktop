use crate::MainWindow;
use crate::context::AppContext;
use crate::phash;
use crate::playback::{
    format_time, show_error, start_item_playback, stats_text, update_texture_size,
};
use adult_hub_player::{Player, PlayerEvent};
use slint::{ComponentHandle, Timer, TimerMode};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_AUTOMATIC_RETRIES: u8 = 3;

#[derive(Default)]
struct PlaybackRecovery {
    retries: u8,
    retry_at: Option<Instant>,
    position_seconds: f64,
    retry_in_flight: bool,
    loaded_at: Option<Instant>,
    source_generation: u64,
}

pub fn install_polling(ui: &MainWindow, ctx: &AppContext, frame_pending: Arc<AtomicBool>) -> Timer {
    let ui_weak = ui.as_weak();
    let timer = Timer::default();
    let mut tick: u64 = 0;

    let player = ctx.player.clone();
    let rep = ctx.reporter.clone();
    let sctrl = ctx.session_ctrl.clone();
    let cur_item = ctx.current_playing_item.clone();
    let ap_ref = ctx.autoplay_active.clone();
    let hd_ref = ctx.hwdec_active.clone();
    let hdrs = ctx.headers.clone();
    let cs = ctx.credential_store.clone();
    let ls = ctx.local_store.clone();
    let jf = ctx.jellyfin.clone();
    let phash_ref = ctx.phash_coord.clone();
    let pending_resume = ctx.pending_resume_position.clone();
    let mut recovery = PlaybackRecovery::default();
    let mut toast_ticks: u32 = 0;

    timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        if frame_pending.swap(false, Ordering::AcqRel) {
            ui.window().request_redraw();
        }
        let Some(player) = &player else { return };

        for event in player.events().try_iter() {
            handle_event(
                &ui,
                player,
                event,
                &rep,
                &sctrl,
                &cur_item,
                &ap_ref,
                &hd_ref,
                &hdrs,
                &cs,
                &ls,
                &jf,
                &phash_ref,
                &pending_resume,
                &mut recovery,
            );
        }

        // Toast auto-dismissal after 4 seconds (40 ticks @ 100ms) for notifications without action button
        if ui.get_toast_visible() && ui.get_toast_action_text().is_empty() {
            toast_ticks += 1;
            if toast_ticks >= 40 {
                ui.set_toast_visible(false);
                toast_ticks = 0;
            }
        } else {
            toast_ticks = 0;
        }

        // Buffering / cache starvation detection
        if ui.get_player_media_loaded() {
            let paused_for_cache = player.get_string("paused-for-cache").as_deref() == Some("yes");
            if paused_for_cache && !player.is_paused() {
                if ui.get_player_state() != "buffering" {
                    ui.set_player_state("buffering".into());
                }
            } else if ui.get_player_state() == "buffering" {
                ui.set_player_state(if player.is_paused() { "paused".into() } else { "playing".into() });
            }
        }

        if let Some(retry_at) = recovery.retry_at
            && Instant::now() >= retry_at
        {
            recovery.retry_at = None;
            if player.load_generation() != recovery.source_generation {
                recovery = PlaybackRecovery::default();
            } else {
                recovery.retry_in_flight = true;
                if let Err(error) = player.retry_current_at(recovery.position_seconds) {
                    recovery.retry_in_flight = false;
                    recovery.loaded_at = None;
                    show_error(&ui, format!("Automatic playback retry failed: {error}"));
                } else {
                    ui.set_status(
                        format!(
                            "Retrying connection ({}/{MAX_AUTOMATIC_RETRIES})…",
                            recovery.retries
                        )
                        .into(),
                    );
                    ui.set_status_color(slint::Color::from_rgb_u8(255, 180, 100));
                }
            }
        }

        let idle = player.get_string("idle-active").as_deref() == Some("yes");
        let paused = player.is_paused() || idle;
        if paused == ui.get_playing() {
            ui.set_playing(!paused);
        }

        if !paused
            && recovery
                .loaded_at
                .is_some_and(|loaded_at| loaded_at.elapsed() >= Duration::from_secs(15))
        {
            recovery.retries = 0;
            recovery.loaded_at = None;
        }

        if let Some(position) = player.position()
            && (position - ui.get_position() as f64).abs() > 0.05
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
        if ui.get_hud_visible() && tick % 5 == 0 {
            ui.set_hud_text(stats_text(&player.stats()).into());
        }

        // Update pHash video matching status in player overlay
        if tick % 5 == 0 {
            if let Some(item) = cur_item.lock().unwrap().as_ref() {
                if let Some(vid) = phash::adult_hub_video_id(item) {
                    if let Some(st) = phash_ref.get_status(&vid) {
                        ui.set_phash_status_text(st.message.into());
                    } else {
                        ui.set_phash_status_text("".into());
                    }
                } else {
                    ui.set_phash_status_text("".into());
                }
            } else {
                ui.set_phash_status_text("".into());
            }
        }

        // Periodic progress checkpoint and reporting every ~10 seconds (~100 ticks at 100ms)
        if tick % 100 == 0 && !idle {
            let pos_ms = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
            let dur_ms = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
            rep.tick_periodic(pos_ms, paused);
            if let Some(item) = cur_item.lock().unwrap().as_ref() {
                sctrl.checkpoint_progress(item, pos_ms, dur_ms, false);
            }
        }
    });

    // Slint timers stop when their handle is dropped. Return it so the caller
    // can keep polling alive for the lifetime of the window.
    timer
}

fn handle_event(
    ui: &MainWindow,
    player: &Player,
    event: PlayerEvent,
    reporter: &Arc<crate::reporter::PlaybackReporter>,
    session_ctrl: &Arc<crate::session::PlaybackSessionController>,
    current_item: &Arc<std::sync::Mutex<Option<adult_hub_client::TvItem>>>,
    autoplay_active: &Arc<AtomicBool>,
    hwdec_active: &Arc<AtomicBool>,
    headers: &[String],
    credential_store: &Arc<adult_hub_storage::CredentialStore>,
    local_store: &Arc<adult_hub_storage::LocalStore>,
    jellyfin: &Arc<adult_hub_client::JellyfinClient>,
    phash_coord: &Arc<phash::PhashCoordinator>,
    pending_resume: &Arc<std::sync::Mutex<Option<f64>>>,
    recovery: &mut PlaybackRecovery,
) {
    match event {
        PlayerEvent::FileLoaded { width, height } => {
            if recovery.retry_in_flight {
                recovery.retry_in_flight = false;
            } else {
                recovery.retries = 0;
                recovery.retry_at = None;
            }
            recovery.loaded_at = Some(Instant::now());
            ui.set_player_media_loaded(true);
            let user_paused_during_loading = ui.get_player_state() == "paused";
            if user_paused_during_loading {
                let _ = player.set_pause(true);
                ui.set_player_state("paused".into());
                ui.set_playing(false);
            } else {
                let _ = player.set_pause(false);
                ui.set_player_state("playing".into());
                ui.set_playing(true);
            }
            ui.set_playback_ended_visible(false);
            ui.set_autoplay_countdown_sec(0);
            update_texture_size(ui, width, height);

            // Position was already passed at mpv loadfile initialization (start=seconds).
            // Avoid redundant seek jumps.
            let _ = pending_resume.lock().unwrap().take();

            let name = player
                .get_string("filename")
                .and_then(|path| {
                    Path::new(&path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "media".to_owned());
            crate::diagnostics::GLOBAL_LOG.i("Player", &format!("Loaded filename: {name}"));
            ui.set_status(format!("Loaded: {name}").into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            ui.set_playing(!player.is_paused());

            let pos_ms = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
            let current_item = current_item.lock().unwrap().clone();
            ui.set_player_title(
                current_item
                    .as_ref()
                    .map(crate::catalog::display_title)
                    .unwrap_or(name)
                    .into(),
            );
            let report_to_jellyfin = current_item
                .as_ref()
                .is_some_and(|item| uuid::Uuid::parse_str(&item.id).is_ok());
            if report_to_jellyfin {
                reporter.report_start(pos_ms, !player.is_paused());
            }

            let dur_ms = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
            if dur_ms > 0 {
                if let Some(item) = current_item.as_ref() {
                    let phash_enabled = local_store
                        .get_setting("phash_on_playback")
                        .ok()
                        .flatten()
                        .map(|v| v == "true" || v == "1")
                        .unwrap_or(false);
                    if phash_enabled
                        && (uuid::Uuid::parse_str(&item.id).is_ok() || item.is_telegram())
                    {
                        phash_coord.enqueue_generation(item, Some(dur_ms));
                    }
                }
            }
        }
        PlayerEvent::VideoReconfig { width, height } => {
            update_texture_size(ui, width, height);
        }
        PlayerEvent::EndFile { eof, error } => {
            ui.set_playing(false);
            if let Some(error) = error {
                crate::diagnostics::GLOBAL_LOG.e("Player", &format!("Playback error: {error}"));
                let err_lower = error.to_lowercase();
                let is_hwdec_err = err_lower.contains("hwdec")
                    || err_lower.contains("decode")
                    || err_lower.contains("codec")
                    || err_lower.contains("hardware");

                if hwdec_active.load(Ordering::SeqCst) && is_hwdec_err {
                    *recovery = PlaybackRecovery::default();
                    hwdec_active.store(false, Ordering::SeqCst);
                    let _ = player.set_hwdec("no");
                    ui.set_hwdec_active(false);
                    ui.set_player_state("loading".into());
                    ui.set_player_loading_message(
                        "Hardware decode failed; retrying with software decode…".into(),
                    );
                    ui.set_status(
                        "Hardware decode failed; retrying with software decode...".into(),
                    );
                    ui.set_status_color(slint::Color::from_rgb_u8(255, 180, 100));

                    if let Some(item) = current_item.lock().unwrap().clone() {
                        start_item_playback(
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
                        );
                        return;
                    } else {
                        // Direct URLs and local files have no catalog item to
                        // resolve again. Retry the remembered libmpv request
                        // after disabling hardware decode so original headers
                        // and the current playback position are preserved.
                        let position = player
                            .position()
                            .filter(|position| position.is_finite() && *position > 0.0)
                            .unwrap_or(ui.get_position() as f64)
                            .max(0.0);
                        recovery.retries = 0;
                        recovery.retry_in_flight = true;
                        recovery.loaded_at = None;
                        recovery.retry_at = None;
                        if let Err(retry_error) = player.retry_current_at(position) {
                            recovery.retry_in_flight = false;
                            show_error(
                                ui,
                                format!("Software decode retry could not start: {retry_error}"),
                            );
                        }
                        return;
                    }
                }

                if is_transient_network_error(&error) && recovery.retries < MAX_AUTOMATIC_RETRIES {
                    recovery.retries += 1;
                    recovery.retry_in_flight = false;
                    recovery.position_seconds = player
                        .position()
                        .filter(|position| *position > 0.0)
                        .unwrap_or(ui.get_position() as f64)
                        .max(0.0);
                    let delay_seconds = 1_u64 << (recovery.retries - 1);
                    recovery.retry_at = Some(Instant::now() + Duration::from_secs(delay_seconds));
                    recovery.source_generation = player.load_generation();
                    recovery.loaded_at = None;
                    ui.set_player_state("loading".into());
                    ui.set_player_loading_message(
                        format!(
                            "Connection interrupted · retrying in {}s…",
                            delay_seconds
                        )
                        .into(),
                    );
                    ui.set_status(
                        format!(
                            "Connection interrupted · retry {}/{} in {}s",
                            recovery.retries, MAX_AUTOMATIC_RETRIES, delay_seconds
                        )
                        .into(),
                    );
                    ui.set_status_color(slint::Color::from_rgb_u8(255, 180, 100));
                    return;
                }

                recovery.retry_at = None;
                recovery.retry_in_flight = false;
                ui.set_player_state("failed".into());
                ui.set_player_error_message(format!("Playback error: {error}").into());
                show_error(ui, format!("Playback error: {error}"));
            } else if eof {
                *recovery = PlaybackRecovery::default();
                crate::diagnostics::GLOBAL_LOG.i("Player", "Playback completed EOF");
                let pos_ms = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
                let dur_ms = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
                if let Some(item) = current_item.lock().unwrap().as_ref() {
                    session_ctrl.checkpoint_progress(item, pos_ms, dur_ms, true);
                }
                reporter.stop_session(pos_ms);

                ui.set_player_state("paused".into());

                ui.set_status("Playback ended.".into());
                ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
                ui.set_playback_ended_visible(true);
                ui.set_autoplay_countdown_sec(if autoplay_active.load(Ordering::SeqCst) {
                    10
                } else {
                    0
                });
            } else {
                recovery.retry_at = None;
                recovery.retry_in_flight = false;
            }
        }
        PlayerEvent::LogMessage {
            level,
            prefix,
            text,
        } => {
            let message = format!("{}: {}", prefix, text);
            if matches!(level.as_str(), "fatal" | "error") {
                crate::diagnostics::GLOBAL_LOG.e("libmpv", &message);
            } else {
                crate::diagnostics::GLOBAL_LOG.w("libmpv", &message);
            }
        }
        PlayerEvent::Shutdown => {}
    }
}

fn is_transient_network_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    if [
        "timed out",
        "timeout",
        "connection reset",
        "connection refused",
        "network is unreachable",
        "network unreachable",
        "no route to host",
        "host is unreachable",
        "temporarily unavailable",
        "temporary failure",
        "resource temporarily unavailable",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return true;
    }

    if !(lower.contains("http") || lower.contains("status") || lower.contains("server")) {
        return false;
    }

    lower
        .split(|character: char| !character.is_ascii_digit())
        .filter_map(|part| part.parse::<u16>().ok())
        .any(|status| status == 408 || status == 429 || (500..=599).contains(&status))
}
