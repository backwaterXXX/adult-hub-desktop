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
    load_requested_at: Option<Instant>,
}

pub fn install_polling(ui: &MainWindow, ctx: &AppContext, frame_pending: Arc<AtomicBool>) -> Timer {
    let ui_weak = ui.as_weak();
    let timer = Timer::default();
    let mut tick: u64 = 0;

    let player = ctx.player.clone();
    let rep = ctx.reporter.clone();
    let sctrl = ctx.session_ctrl.clone();
    let cur_item = ctx.current_playing_item.clone();
    let phash_ref = ctx.phash_coord.clone();
    let download_mgr = ctx.download_manager.clone();
    let ctx_handle = ctx.clone();
    let mut recovery = PlaybackRecovery::default();
    let mut toast_ticks: u32 = 0;

    timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        if frame_pending.swap(false, Ordering::AcqRel) {
            ui.window().request_redraw();
        }
        let Some(player) = &player else { return };

        for event in player.events().try_iter() {
            handle_event(&ui, player, event, &ctx_handle, &mut recovery);
        }

        // Stall guard: mpv accepted the load request but never reported the
        // file loaded (e.g. its video output failed to initialise). Surface a
        // failure instead of spinning on "Opening media stream…" forever.
        if ui.get_player_state() == "loading" && !ui.get_player_media_loaded() {
            let stalled = match recovery.load_requested_at {
                Some(since) => since.elapsed() >= Duration::from_secs(30),
                None => {
                    recovery.load_requested_at = Some(Instant::now());
                    false
                }
            };
            if stalled {
                crate::log_debug(
                    "playback: stalled 30s in loading without FileLoaded, stopping mpv",
                );
                recovery.load_requested_at = None;
                recovery.retry_in_flight = false;
                recovery.retry_at = None;
                let _ = player.stop();
                ui.set_player_state("failed".into());
                crate::diagnostics::GLOBAL_LOG
                    .e("Player", "Load stalled: mpv never reported the file loaded");
                crate::playback::show_playback_error(
                    &ui,
                    "The player did not start playback within 30 seconds",
                    "mpv accepted the file but never loaded it. Its video output may have \
                     failed to initialise; try the other renderer in Settings.",
                );
            }
        } else {
            recovery.load_requested_at = None;
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

        // Synchronize download task status to UI
        if let Some(task) = download_mgr.current_task() {
            let status = task.status.lock().unwrap().clone();
            let bytes = task.bytes_downloaded.load(Ordering::Relaxed);
            let total = task.total_bytes.load(Ordering::Relaxed);
            ui.set_download_can_pause(task.supports_range.load(Ordering::Relaxed));

            match status {
                crate::download::DownloadStatus::Idle => {
                    if ui.get_download_status() != "idle" {
                        ui.set_download_status("idle".into());
                        ui.set_download_speed_text("".into());
                    }
                }
                crate::download::DownloadStatus::Queued => {
                    ui.set_download_status("queued".into());
                    ui.set_download_item_id(task.item_id.clone().into());
                    ui.set_download_title(task.item_title.clone().into());
                    ui.set_download_indeterminate(true);
                    ui.set_download_progress(0.0);
                    ui.set_download_bytes_text("Waiting in queue…".into());
                    ui.set_download_speed_text("".into());
                }
                crate::download::DownloadStatus::Preparing => {
                    ui.set_download_status("preparing".into());
                    ui.set_download_item_id(task.item_id.clone().into());
                    ui.set_download_title(task.item_title.clone().into());
                    ui.set_download_indeterminate(true);
                    ui.set_download_progress(0.0);
                    ui.set_download_bytes_text("Preparing video stream…".into());
                    ui.set_download_speed_text("".into());
                }
                crate::download::DownloadStatus::Downloading => {
                    ui.set_download_status("downloading".into());
                    ui.set_download_item_id(task.item_id.clone().into());
                    ui.set_download_title(task.item_title.clone().into());

                    let (speed_bps, eta_opt) = download_mgr.sample_speed(bytes, total);
                    let speed_str = crate::download::format_download_speed(speed_bps);
                    let speed_display = match (speed_str.is_empty(), eta_opt) {
                        (false, Some(eta)) => format!("{speed_str} · {eta}"),
                        (false, None) => speed_str,
                        (true, _) => String::new(),
                    };
                    ui.set_download_speed_text(speed_display.into());

                    if total > 0 {
                        let prog = (bytes as f32 / total as f32).clamp(0.0, 1.0);
                        ui.set_download_progress(prog);
                        ui.set_download_indeterminate(false);
                        let pct = (prog * 100.0).round() as u32;
                        let b_text = format!(
                            "{} ({}%)",
                            crate::download::format_download_bytes(bytes, total),
                            pct
                        );
                        ui.set_download_bytes_text(b_text.into());
                    } else {
                        ui.set_download_progress(0.0);
                        ui.set_download_indeterminate(true);
                        ui.set_download_bytes_text(
                            format!(
                                "{} (unknown size)",
                                crate::download::format_download_bytes(bytes, 0)
                            )
                            .into(),
                        );
                    }
                }
                crate::download::DownloadStatus::Paused => {
                    ui.set_download_status("paused".into());
                    ui.set_download_item_id(task.item_id.clone().into());
                    ui.set_download_title(task.item_title.clone().into());
                    ui.set_download_speed_text("Paused".into());
                    if total > 0 {
                        let prog = (bytes as f32 / total as f32).clamp(0.0, 1.0);
                        ui.set_download_progress(prog);
                        ui.set_download_indeterminate(false);
                        let pct = (prog * 100.0).round() as u32;
                        ui.set_download_bytes_text(
                            format!(
                                "{} ({}% - Paused)",
                                crate::download::format_download_bytes(bytes, total),
                                pct
                            )
                            .into(),
                        );
                    } else {
                        ui.set_download_bytes_text(
                            format!(
                                "{} (Paused)",
                                crate::download::format_download_bytes(bytes, 0)
                            )
                            .into(),
                        );
                    }
                }
                crate::download::DownloadStatus::Completed { ref file_path, .. } => {
                    if ui.get_download_status() != "completed" {
                        ui.set_download_status("completed".into());
                        ui.set_download_progress(1.0);
                        ui.set_download_indeterminate(false);
                        ui.set_download_speed_text("".into());
                        ui.set_download_file_path(file_path.to_string_lossy().to_string().into());
                        ui.set_download_bytes_text(
                            format!(
                                "Downloaded: {}",
                                crate::download::format_download_bytes(bytes, bytes)
                            )
                            .into(),
                        );
                    }
                }
                crate::download::DownloadStatus::Error(ref err) => {
                    if ui.get_download_status() != "error" {
                        ui.set_download_status("error".into());
                        ui.set_download_speed_text("".into());
                        ui.set_download_error_text(err.clone().into());
                    }
                }
            }
        }

        // Periodic sync of download queue and history (every 500ms / 5 ticks)
        if tick % 5 == 0 {
            let queued = download_mgr.queued_tasks();
            ui.set_download_queue_count(queued.len() as i32);
            let queue_entries: Vec<crate::DownloadQueueEntry> = queued
                .into_iter()
                .map(|t| crate::DownloadQueueEntry {
                    item_id: t.item_id.clone().into(),
                    title: t.item_title.clone().into(),
                    status_text: "Waiting".into(),
                })
                .collect();
            ui.set_download_queue(std::rc::Rc::new(slint::VecModel::from(queue_entries)).into());

            let history = download_mgr.history_items();
            let history_entries: Vec<crate::DownloadHistoryEntry> = history
                .into_iter()
                .map(|h| crate::DownloadHistoryEntry {
                    item_id: h.item_id.into(),
                    title: h.title.into(),
                    file_path: h.file_path.to_string_lossy().to_string().into(),
                    size_text: crate::download::format_download_bytes(
                        h.file_size_bytes,
                        h.file_size_bytes,
                    )
                    .into(),
                })
                .collect();
            ui.set_download_history(
                std::rc::Rc::new(slint::VecModel::from(history_entries)).into(),
            );
        }

        // Buffering / cache starvation detection
        if ui.get_player_media_loaded() {
            let paused_for_cache = player.get_string("paused-for-cache").as_deref() == Some("yes");
            if paused_for_cache && !player.is_paused() {
                if ui.get_player_state() != "buffering" {
                    ui.set_player_state("buffering".into());
                }
            } else if ui.get_player_state() == "buffering" {
                ui.set_player_state(if player.is_paused() {
                    "paused".into()
                } else {
                    "playing".into()
                });
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
    ctx: &crate::context::AppContext,
    recovery: &mut PlaybackRecovery,
) {
    let reporter = &ctx.reporter;
    let session_ctrl = &ctx.session_ctrl;
    let current_item = &ctx.current_playing_item;
    let hwdec_active = &ctx.hwdec_active;
    let headers = &ctx.headers;
    let credential_store = &ctx.credential_store;
    let local_store = &ctx.local_store;
    let jellyfin = &ctx.jellyfin;
    let phash_coord = &ctx.phash_coord;
    let pending_resume = &ctx.pending_resume_position;
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
            crate::log_debug(&format!(
                "playback: FileLoaded {width:?}x{height:?}, retry_in_flight={}",
                recovery.retry_in_flight
            ));
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

            // Enforce pending resume position if libmpv did not seek automatically on loadfile
            let resume_target = pending_resume.lock().unwrap().take();
            if let Some(target_sec) = resume_target {
                if target_sec > 0.0 {
                    let cur_sec = player.position().unwrap_or(0.0);
                    if (cur_sec - target_sec).abs() > 1.5 {
                        let _ = player.seek_absolute(target_sec);
                    }
                    ui.set_position(target_sec as f32);
                    ui.set_position_text(format_time(target_sec).into());
                }
            }

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

            let pos_ms = if let Some(target) = resume_target {
                (target * 1000.0) as u64
            } else {
                (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64
            };
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
            crate::log_debug(&format!(
                "playback: EndFile eof={eof} error={}",
                error.as_deref().unwrap_or("none")
            ));
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
                    crate::log_debug(&format!(
                        "playback: transient network error, retry {}/{} in {}s",
                        recovery.retries, MAX_AUTOMATIC_RETRIES, delay_seconds
                    ));
                    recovery.retry_at = Some(Instant::now() + Duration::from_secs(delay_seconds));
                    recovery.source_generation = player.load_generation();
                    recovery.loaded_at = None;
                    ui.set_player_state("loading".into());
                    ui.set_player_loading_message(
                        format!("Connection interrupted · retrying in {}s…", delay_seconds).into(),
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
                let finished_item = current_item.lock().unwrap().take();
                if let Some(ref item) = finished_item {
                    session_ctrl.checkpoint_progress(item, pos_ms, dur_ms, true);
                }
                reporter.stop_session(pos_ms);

                // User requirement:
                // - Do NOT display an end screen, card, or countdown dialog asking for confirmation.
                // - Automatically play the next contextual video.
                ui.set_playback_ended_visible(false);
                ui.set_autoplay_countdown_sec(0);

                let current_id = finished_item.as_ref().map(|it| it.id.as_str());
                if let Some(next_item) = ctx.get_next_contextual_item(current_id) {
                    ui.set_status(format!("Playing next: {}", next_item.title).into());
                    crate::playback::start_item_playback_with_resume_ctx(
                        ui,
                        ctx,
                        next_item,
                        Some(false),
                    );
                } else {
                    ui.invoke_play_random();
                }
            } else {
                recovery.retry_at = None;
                recovery.retry_in_flight = false;
                // Software UI mode plays into mpv's own window; closing that
                // window is a clean non-EOF stop the main UI cannot see any
                // other way. Bring the application to the same consistent
                // stopped state as navigating away from the player.
                if ui.get_software_ui_renderer() && ui.get_player_media_loaded() {
                    crate::playback::stop_active_playback_for_navigation(
                        ui,
                        Some(player),
                        reporter,
                        session_ctrl,
                        current_item,
                    );
                    ui.set_status("Video window closed".into());
                    ui.set_status_color(slint::Color::from_rgb_u8(158, 158, 158));
                }
            }
        }
        PlayerEvent::LogMessage {
            level,
            prefix,
            text,
        } => {
            let message = format!("{}: {}", prefix, text);
            if matches!(level.as_str(), "fatal" | "error") {
                eprintln!("libmpv {level}: {message}");
                crate::diagnostics::GLOBAL_LOG.e("libmpv", &message);
            } else {
                crate::diagnostics::GLOBAL_LOG.w("libmpv", &message);
            }
        }
        PlayerEvent::Shutdown => {
            crate::log_debug("playback: mpv sent Shutdown event");
        }
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
