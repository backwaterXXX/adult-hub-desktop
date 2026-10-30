use crate::MainWindow;
use crate::context::AppContext;
use slint::ComponentHandle;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn install_stage9_handlers(ui: &MainWindow, ctx: &AppContext) {
    let player = ctx.player.clone();

    // 1. Video Canvas Clicked (Double-click -> Fullscreen, Single-click -> Play/Pause)
    {
        let weak = ui.as_weak();
        let last_click = Arc::new(Mutex::new(
            std::time::Instant::now() - Duration::from_secs(10),
        ));
        ui.on_video_canvas_clicked(move || {
            let Some(ui) = weak.upgrade() else { return };
            let now = std::time::Instant::now();
            let mut last = last_click.lock().unwrap();
            if now.duration_since(*last) < Duration::from_millis(350) {
                // Double click: toggle fullscreen
                *last = now - Duration::from_secs(10);
                ui.invoke_toggle_fullscreen();
                ui.invoke_toggle_play(); // revert single click play toggle so playback state persists
            } else {
                *last = now;
                ui.invoke_toggle_play();
            }
        });
    }

    // 2. Adjust Volume (Arrow keys or gestures: +/- 5%)
    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_adjust_volume(move |delta| {
            let Some(ui) = weak.upgrade() else { return };
            let cur = ui.get_volume();
            let new_vol = (cur + delta).clamp(0.0, 100.0);
            if let Some(player) = &player {
                let _ = player.set_volume(new_vol as f64);
                if new_vol > 0.0 && (ui.get_muted() || player.is_muted()) {
                    let _ = player.set_muted(false);
                    ui.set_muted(false);
                }
            }
            ui.set_volume(new_vol);
        });
    }

    // 3. Mouse Wheel Scrolled over Canvas
    {
        let weak = ui.as_weak();
        ui.on_mouse_scrolled(move |delta_y| {
            let Some(ui) = weak.upgrade() else { return };
            let delta = if delta_y > 0.0 { 5.0 } else { -5.0 };
            ui.invoke_adjust_volume(delta);
        });
    }
}
