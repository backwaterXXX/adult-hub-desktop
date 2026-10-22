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
use adult_hub_client::{ServiceUrlPolicy, ServiceUrlValidation};
use adult_hub_storage::LocalStore;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub fn install_all_handlers(
    ui: &MainWindow,
    ctx: &AppContext,
    frame_pending: Arc<AtomicBool>,
) -> slint::Timer {
    controls::install_controls(ui, ctx);
    controls::install_fullscreen(ui);
    browsing::install_stage2_handlers(ui, ctx);
    settings::install_stage4_handlers(ui, ctx);
    updater::install_stage5_handlers(ui, ctx);
    details::install_stage6_handlers(ui, ctx);
    profiles::install_stage7_handlers(ui, ctx);
    channels::install_stage8_handlers(ui, ctx);
    htpc::install_stage9_handlers(ui, ctx);
    polling::install_polling(ui, ctx, frame_pending)
}

pub fn request_cleartext_consent(
    ui: &MainWindow,
    local_store: &LocalStore,
    raw_url: &str,
    action: &str,
) -> bool {
    let service_url = match ServiceUrlPolicy::inspect(raw_url, false) {
        ServiceUrlValidation::Valid(url) => url,
        ServiceUrlValidation::Invalid(error) => {
            ui.set_status(error.into());
            ui.set_status_color(slint::Color::from_rgb_u8(244, 67, 54));
            return false;
        }
    };
    if !service_url.uses_cleartext
        || local_store
            .approved_cleartext_origins()
            .is_ok_and(|origins| origins.contains(&service_url.origin))
    {
        return true;
    }

    ui.set_cleartext_consent_url(service_url.normalized.into());
    ui.set_cleartext_consent_action(action.into());
    ui.set_cleartext_consent_visible(true);
    ui.set_status("Confirm the unencrypted connection to continue".into());
    ui.set_status_color(slint::Color::from_rgb_u8(255, 190, 90));
    false
}

pub fn refresh_approved_http_origins(ui: &MainWindow, local_store: &LocalStore) {
    let origins = local_store.approved_cleartext_origins().unwrap_or_default();
    ui.set_approved_http_origins(
        Rc::new(slint::VecModel::from(
            origins
                .into_iter()
                .map(slint::SharedString::from)
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
}
