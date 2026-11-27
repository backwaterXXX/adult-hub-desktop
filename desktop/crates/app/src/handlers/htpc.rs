use crate::MainWindow;
use crate::context::AppContext;
use slint::ComponentHandle;
use std::time::Duration;

pub fn install_stage9_handlers(ui: &MainWindow, ctx: &AppContext) {
    let player = ctx.player.clone();

    // 1. Video Canvas Clicked (Double-click -> Fullscreen, Single-click -> Play/Pause)
    {
        const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(350);
        let weak = ui.as_weak();
        let pending_single_click: std::rc::Rc<std::cell::RefCell<Option<slint::Timer>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        ui.on_video_canvas_clicked(move || {
            let Some(ui) = weak.upgrade() else { return };
            if pending_single_click.borrow().is_some() {
                // Second click within window: cancel pending play/pause and toggle fullscreen.
                pending_single_click.borrow_mut().take();
                ui.invoke_toggle_fullscreen();
            } else {
                let timer = slint::Timer::default();
                let weak_single = ui.as_weak();
                let pending_ref = pending_single_click.clone();
                timer.start(
                    slint::TimerMode::SingleShot,
                    DOUBLE_CLICK_WINDOW,
                    move || {
                        pending_ref.borrow_mut().take();
                        if let Some(ui) = weak_single.upgrade() {
                            ui.invoke_toggle_play();
                        }
                    },
                );
                *pending_single_click.borrow_mut() = Some(timer);
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
