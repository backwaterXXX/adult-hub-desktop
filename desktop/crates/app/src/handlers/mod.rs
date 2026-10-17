pub mod browsing;
pub mod channels;
pub mod controls;
pub mod details;
pub mod htpc;
pub mod polling;
pub mod profiles;
pub mod settings;
pub mod updater;

use crate::MainWindow;
use crate::context::AppContext;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub fn install_all_handlers(ui: &MainWindow, ctx: &AppContext, frame_pending: Arc<AtomicBool>) {
    controls::install_controls(ui, ctx);
    controls::install_fullscreen(ui);
    browsing::install_stage2_handlers(ui, ctx);
    settings::install_stage4_handlers(ui, ctx);
    updater::install_stage5_handlers(ui, ctx);
    details::install_stage6_handlers(ui, ctx);
    profiles::install_stage7_handlers(ui, ctx);
    channels::install_stage8_handlers(ui, ctx);
    htpc::install_stage9_handlers(ui, ctx);
    polling::install_polling(ui, ctx, frame_pending);
}
