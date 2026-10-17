use crate::MainWindow;
use crate::context::AppContext;
use crate::phash;
use crate::playback::{
    format_time, play_random_catalog_item, show_error, start_item_playback, stats_text,
    update_texture_size,
};
use adult_hub_player::{Player, PlayerEvent};
use slint::{ComponentHandle, Timer, TimerMode};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub fn install_polling(ui: &MainWindow, ctx: &AppContext, frame_pending: Arc<AtomicBool>) {
    let ui_weak = ui.as_weak();
    let timer = Timer::default();
    let mut tick: u64 = 0;

    let player = ctx.player.clone();
    let rep = ctx.reporter.clone();
    let sctrl = ctx.session_ctrl.clone();
    let cur_item = ctx.current_playing_item.clone();
    let cat_ref = ctx.cached_catalog.clone();
    let ap_ref = ctx.autoplay_active.clone();
    let hd_ref = ctx.hwdec_active.clone();
    let hdrs = ctx.headers.clone();
    let cs = ctx.credential_store.clone();
    let ls = ctx.local_store.clone();
    let jf = ctx.jellyfin.clone();
    let phash_ref = ctx.phash_coord.clone();

    timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        if frame_pending.swap(false, Ordering::AcqRel) {
            ui.window().request_redraw();
        }
        let Some(player) = &player else { return };

        for event in player.events().try_iter() {
            handle_event(
                &ui, player, event, &rep, &sctrl, &cur_item, &cat_ref, &ap_ref, &hd_ref, &hdrs,
                &cs, &ls, &jf, &phash_ref,
            );
        }

        let idle = player.get_string("idle-active").as_deref() == Some("yes");
        let paused = player.is_paused() || idle;
        if paused == ui.get_playing() {
            ui.set_playing(!paused);
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
}

pub fn handle_event(
    ui: &MainWindow,
    player: &Player,
    event: PlayerEvent,
    reporter: &Arc<crate::reporter::PlaybackReporter>,
    session_ctrl: &Arc<crate::session::PlaybackSessionController>,
    current_item: &Arc<std::sync::Mutex<Option<adult_hub_client::TvItem>>>,
    cached_catalog: &Arc<std::sync::Mutex<Vec<adult_hub_client::TvItem>>>,
    autoplay_active: &Arc<AtomicBool>,
    hwdec_active: &Arc<AtomicBool>,
    headers: &[String],
    credential_store: &Arc<adult_hub_storage::CredentialStore>,
    local_store: &Arc<adult_hub_storage::LocalStore>,
    jellyfin: &Arc<adult_hub_client::JellyfinClient>,
    phash_coord: &Arc<phash::PhashCoordinator>,
) {
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
            crate::diagnostics::GLOBAL_LOG.i("Player", &format!("Loaded filename: {name}"));
            ui.set_status(format!("Loaded: {name}").into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            ui.set_playing(!player.is_paused());

            let pos_ms = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
            reporter.report_start(pos_ms, !player.is_paused());

            let dur_ms = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
            if dur_ms > 0 {
                if let Some(item) = current_item.lock().unwrap().as_ref() {
                    let phash_enabled = local_store
                        .get_setting("phash_on_playback")
                        .ok()
                        .flatten()
                        .map(|v| v == "true" || v == "1")
                        .unwrap_or(false);
                    if phash_enabled {
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
                    hwdec_active.store(false, Ordering::SeqCst);
                    let _ = player.set_hwdec("no");
                    ui.set_hwdec_active(false);
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
                    }
                }
                show_error(ui, format!("Playback error: {error}"));
            } else if eof {
                crate::diagnostics::GLOBAL_LOG.i("Player", "Playback completed EOF");
                let pos_ms = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
                let dur_ms = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
                if let Some(item) = current_item.lock().unwrap().as_ref() {
                    session_ctrl.checkpoint_progress(item, pos_ms, dur_ms, true);
                }
                reporter.stop_session(pos_ms);

                ui.set_status("Playback ended.".into());
                ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));

                if autoplay_active.load(Ordering::SeqCst) {
                    play_random_catalog_item(
                        ui,
                        player,
                        headers,
                        credential_store,
                        local_store,
                        jellyfin,
                        reporter,
                        session_ctrl,
                        current_item,
                        cached_catalog,
                        phash_coord,
                    );
                }
            }
        }
        PlayerEvent::Shutdown => {}
    }
}
