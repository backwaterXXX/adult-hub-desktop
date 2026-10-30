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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallCause {
    BufferLimitTooSmall,
    TransientBandwidthDrop,
    BandwidthDeficit,
    SeekOrReopenCost,
    DecodingIssue,
    WaitingForSource,
    Unknown,
}

impl StallCause {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BufferLimitTooSmall => "BufferLimitTooSmall",
            Self::TransientBandwidthDrop => "TransientBandwidthDrop",
            Self::BandwidthDeficit => "BandwidthDeficit",
            Self::SeekOrReopenCost => "SeekOrReopenCost",
            Self::DecodingIssue => "DecodingIssue",
            Self::WaitingForSource => "WaitingForSource",
            Self::Unknown => "Unknown",
        }
    }
}

pub fn classify_stall_cause(
    start_time: Instant,
    last_seek_time: Option<Instant>,
    last_load_time: Option<Instant>,
    dropped_frames_diff: u64,
    stall_start_fw_bytes: u64,
    stall_start_cache_secs: f64,
    stall_start_video_bitrate: Option<f64>,
    stall_start_raw_input_rate: Option<f64>,
    current_input_rate: Option<f64>,
    active_profile: adult_hub_player::BufferProfile,
) -> StallCause {
    if let Some(seek_time) = last_seek_time
        && start_time.saturating_duration_since(seek_time) <= Duration::from_millis(4000)
    {
        StallCause::SeekOrReopenCost
    } else if let Some(load_time) = last_load_time
        && start_time.saturating_duration_since(load_time) <= Duration::from_millis(5000)
    {
        StallCause::SeekOrReopenCost
    } else if dropped_frames_diff >= 5 {
        StallCause::DecodingIssue
    } else {
        let max_bytes_limit: u64 = match active_profile {
            adult_hub_player::BufferProfile::FastStart => 48 * 1024 * 1024,
            adult_hub_player::BufferProfile::Standard => 160 * 1024 * 1024,
            adult_hub_player::BufferProfile::Large => 384 * 1024 * 1024,
            adult_hub_player::BufferProfile::LowMemory => 24 * 1024 * 1024,
        };
        if stall_start_fw_bytes >= (max_bytes_limit * 85 / 100) && stall_start_cache_secs < 8.0 {
            StallCause::BufferLimitTooSmall
        } else if let (Some(video_bitrate), Some(start_rate), Some(curr_rate)) = (
            stall_start_video_bitrate.filter(|&v| v > 0.0),
            stall_start_raw_input_rate,
            current_input_rate,
        ) {
            let start_bps = start_rate * 8.0;
            let curr_bps = curr_rate * 8.0;
            if start_bps < (video_bitrate * 0.8) && curr_bps < (video_bitrate * 0.8) {
                StallCause::BandwidthDeficit
            } else {
                StallCause::TransientBandwidthDrop
            }
        } else if stall_start_cache_secs <= 0.2 {
            StallCause::WaitingForSource
        } else {
            StallCause::Unknown
        }
    }
}

#[derive(Default)]
pub struct StallTracker {
    pub count: u32,
    pub total_duration: Duration,
    pub stall_start: Option<Instant>,
    pub stall_start_dropped_frames: u64,
    pub stall_start_fw_bytes: u64,
    pub stall_start_cache_secs: f64,
    pub stall_start_raw_input_rate: Option<f64>,
    pub stall_start_video_bitrate: Option<f64>,
    pub active_generation: u64,
}

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
    let mut stall_tracker = StallTracker::default();
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

        // Buffering / cache starvation detection & diagnostics
        if ui.get_player_media_loaded() {
            let cur_gen = player.load_generation();
            if cur_gen != stall_tracker.active_generation {
                stall_tracker = StallTracker {
                    active_generation: cur_gen,
                    ..Default::default()
                };
            }

            let paused_for_cache = player.get_string("paused-for-cache").as_deref() == Some("yes");
            let req_id = ctx_handle.session_ctrl.current_request_id();

            if paused_for_cache && !player.is_paused() {
                if ui.get_player_state() != "buffering" {
                    ui.set_player_state("buffering".into());
                }

                if stall_tracker.stall_start.is_none() {
                    let now = Instant::now();
                    stall_tracker.stall_start = Some(now);
                    stall_tracker.count += 1;

                    let stats = player.stats();
                    stall_tracker.stall_start_dropped_frames = stats.dropped_frames.unwrap_or(0);
                    stall_tracker.stall_start_fw_bytes = stats.cache_forward_bytes.unwrap_or(0);
                    stall_tracker.stall_start_cache_secs = stats.cache_duration_secs.unwrap_or(0.0);
                    stall_tracker.stall_start_raw_input_rate = stats.raw_input_rate_bps;
                    stall_tracker.stall_start_video_bitrate = stats.video_bitrate;

                    let dl_rate_str = match stats.raw_input_rate_bps {
                        Some(rate) => {
                            let mbps = (rate * 8.0) / 1_000_000.0;
                            let mbs = rate / (1024.0 * 1024.0);
                            format!("{mbps:.2} Mbit/s ({mbs:.2} MB/s)")
                        }
                        None => "N/A".to_string(),
                    };
                    let vbitrate_str = match stats.video_bitrate {
                        Some(vb) => format!("{:.2} Mbit/s", vb / 1_000_000.0),
                        None => "N/A".to_string(),
                    };

                    crate::log_debug(&format!(
                        "playback req={req_id}: stall #{} started: pos={:.2}s, video_bitrate={}, dl_rate={}, cache_secs={:.2}s, cache_fw_bytes={}",
                        stall_tracker.count,
                        player.position().unwrap_or(0.0),
                        vbitrate_str,
                        dl_rate_str,
                        stall_tracker.stall_start_cache_secs,
                        stall_tracker.stall_start_fw_bytes,
                    ));
                    crate::diagnostics::GLOBAL_LOG.w(
                        "Player",
                        &format!(
                            "Playback stall #{} started (req={}): buffer cache starved (0.0s), dl_rate={}, vbitrate={}",
                            stall_tracker.count,
                            req_id,
                            dl_rate_str,
                            vbitrate_str,
                        ),
                    );
                }
            } else {
                if let Some(start_time) = stall_tracker.stall_start.take() {
                    let stall_duration = start_time.elapsed();
                    stall_tracker.total_duration += stall_duration;

                    let stats = player.stats();
                    let current_dropped = stats.dropped_frames.unwrap_or(0);
                    let dropped_diff = current_dropped.saturating_sub(stall_tracker.stall_start_dropped_frames);
                    let current_input_rate = stats.raw_input_rate_bps;
                    let active_profile = player.buffer_profile();

                    let cause = classify_stall_cause(
                        start_time,
                        player.last_seek_time(),
                        player.last_load_time(),
                        dropped_diff,
                        stall_tracker.stall_start_fw_bytes,
                        stall_tracker.stall_start_cache_secs,
                        stall_tracker.stall_start_video_bitrate,
                        stall_tracker.stall_start_raw_input_rate,
                        current_input_rate,
                        active_profile,
                    );

                    let diag_str = cause.as_str().to_string();
                    player.update_stall_summary(
                        stall_tracker.count,
                        stall_tracker.total_duration.as_secs_f64(),
                        Some(diag_str.clone()),
                    );

                    crate::log_debug(&format!(
                        "playback req={req_id}: stall #{} resolved in {:.2}s (total stalled: {:.2}s), cause: {diag_str}",
                        stall_tracker.count,
                        stall_duration.as_secs_f64(),
                        stall_tracker.total_duration.as_secs_f64(),
                    ));
                    crate::diagnostics::GLOBAL_LOG.i(
                        "Player",
                        &format!(
                            "Playback stall #{} resolved in {:.2}s (cause: {diag_str}, total stalled: {:.2}s)",
                            stall_tracker.count,
                            stall_duration.as_secs_f64(),
                            stall_tracker.total_duration.as_secs_f64(),
                        ),
                    );
                }

                if ui.get_player_state() == "buffering" {
                    ui.set_player_state(if player.is_paused() {
                        "paused".into()
                    } else {
                        "playing".into()
                    });
                }
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

        if ui.get_is_switching_movie() && ui.get_player_media_loaded() && ui.get_software_ui_renderer() {
            ui.set_is_switching_movie(false);
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
            if ui.get_software_ui_renderer() && ui.get_is_switching_movie() {
                ui.set_is_switching_movie(false);
            }
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

                ui.invoke_play_next();
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

#[cfg(test)]
mod tests {
    use super::*;
    use adult_hub_player::BufferProfile;
    use std::time::{Duration, Instant};

    #[test]
    fn test_classify_stall_cause_seek_reopen() {
        let now = Instant::now();
        let seek_time = now - Duration::from_millis(2000);
        let cause = classify_stall_cause(
            now,
            Some(seek_time),
            None,
            0,
            0,
            0.0,
            Some(20_000_000.0),
            Some(15_000_000.0),
            Some(15_000_000.0),
            BufferProfile::Standard,
        );
        assert_eq!(cause, StallCause::SeekOrReopenCost);

        let load_time = now - Duration::from_millis(3000);
        let cause_load = classify_stall_cause(
            now,
            None,
            Some(load_time),
            0,
            0,
            0.0,
            Some(20_000_000.0),
            Some(15_000_000.0),
            Some(15_000_000.0),
            BufferProfile::Standard,
        );
        assert_eq!(cause_load, StallCause::SeekOrReopenCost);
    }

    #[test]
    fn test_classify_stall_cause_decoding_issue() {
        let now = Instant::now();
        let cause = classify_stall_cause(
            now,
            None,
            None,
            7, // 7 dropped frames >= 5
            0,
            10.0,
            Some(20_000_000.0),
            Some(25_000_000.0),
            Some(25_000_000.0),
            BufferProfile::Standard,
        );
        assert_eq!(cause, StallCause::DecodingIssue);
    }

    #[test]
    fn test_classify_stall_cause_buffer_limit_too_small() {
        let now = Instant::now();
        // Standard limit: 160 MB. 85% is 136 MB. 140 MB >= 85%, cache_secs 5.0 < 8.0s
        let bytes = 140 * 1024 * 1024;
        let cause = classify_stall_cause(
            now,
            None,
            None,
            0,
            bytes,
            5.0,
            Some(30_000_000.0),
            Some(35_000_000.0),
            Some(35_000_000.0),
            BufferProfile::Standard,
        );
        assert_eq!(cause, StallCause::BufferLimitTooSmall);
    }

    #[test]
    fn test_classify_stall_cause_bandwidth_deficit() {
        let now = Instant::now();
        // 25 Mbps video bitrate, download rate is only 1 MB/s (8 Mbps) < 0.8 * 25 Mbps (20 Mbps)
        let cause = classify_stall_cause(
            now,
            None,
            None,
            0,
            10 * 1024 * 1024,
            2.0,
            Some(25_000_000.0),
            Some(1_000_000.0), // 8 Mbps raw input rate
            Some(1_000_000.0),
            BufferProfile::Standard,
        );
        assert_eq!(cause, StallCause::BandwidthDeficit);
    }

    #[test]
    fn test_classify_stall_cause_transient_drop() {
        let now = Instant::now();
        // Video bitrate is 10 Mbps, normal input rate was 15 Mbps, current input rate recovered to 12 Mbps
        let cause = classify_stall_cause(
            now,
            None,
            None,
            0,
            10 * 1024 * 1024,
            3.0,
            Some(10_000_000.0),
            Some(1_875_000.0), // 15 Mbps
            Some(1_500_000.0), // 12 Mbps
            BufferProfile::Standard,
        );
        assert_eq!(cause, StallCause::TransientBandwidthDrop);
    }

    #[test]
    fn test_classify_stall_cause_missing_bandwidth_does_not_claim_deficit() {
        let now = Instant::now();
        // When download speed is not measured (None), it must NOT classify as BandwidthDeficit!
        let cause_starved = classify_stall_cause(
            now,
            None,
            None,
            0,
            0,
            0.0, // Cache starved
            Some(28_000_000.0),
            None, // dl_rate N/A
            None,
            BufferProfile::Standard,
        );
        assert_eq!(cause_starved, StallCause::WaitingForSource);

        let cause_unknown = classify_stall_cause(
            now,
            None,
            None,
            0,
            5 * 1024 * 1024,
            2.5, // Non-empty cache
            Some(28_000_000.0),
            None, // dl_rate N/A
            None,
            BufferProfile::Standard,
        );
        assert_eq!(cause_unknown, StallCause::Unknown);
    }
}
