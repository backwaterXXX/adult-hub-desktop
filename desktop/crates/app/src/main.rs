#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(clippy::too_many_arguments)]

mod catalog;
mod context;
mod diagnostics;
mod download;
mod handlers;
mod options;
mod phash;
mod playback;
mod renderer;
mod reporter;
mod session;
pub mod shuffle;
mod system_info;
mod thumbnails;
mod updater;

use adult_hub_client::{
    JellyfinClient, ResolverClient, ServiceUrlPolicy, ServiceUrlValidation, discover_servers,
};
use adult_hub_player::Player;
use adult_hub_storage::{CredentialStore, LocalStore, Session, default_db_path};
use catalog::{sample_catalog_tv_items, set_catalog, set_discovered_servers};
use context::AppContext;
use options::{Options, initial_media};
use playback::{load_media, show_error, start_item_playback};
use renderer::install_renderer;
use reporter::PlaybackReporter;
use session::PlaybackSessionController;
use slint::Model;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

slint::include_modules!();

pub use handlers::controls::ASPECTS;

const LICENSES_TEXT: &str = include_str!("../../../../NOTICES.txt");

thread_local! {
    static APP_CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

pub(crate) fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    APP_CLIPBOARD.with(|clipboard| {
        let mut clipboard = clipboard.borrow_mut();
        if clipboard.is_none() {
            *clipboard = Some(arboard::Clipboard::new().map_err(|error| error.to_string())?);
        }
        clipboard
            .as_mut()
            .expect("clipboard initialized above")
            .set_text(text)
            .map_err(|error| error.to_string())
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "windows")]
    unsafe {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn AttachConsole(dwProcessId: u32) -> i32;
        }
        const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
        AttachConsole(ATTACH_PARENT_PROCESS);
    }

    let options = Options::parse(std::env::args().skip(1))?;

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .require_opengl()
        .select()?;

    let ui = MainWindow::new()?;
    install_card_zoom_event_filter(&ui);
    if let Ok(size_str) = std::env::var("AH_WINDOW_SIZE") {
        if size_str == "960x640" {
            ui.set_window_target_width(960.0);
            ui.set_window_target_height(640.0);
        } else if size_str == "1180x790" {
            ui.set_window_target_width(1180.0);
            ui.set_window_target_height(790.0);
        }
    }
    set_window_icon(&ui);
    ui.set_licenses_text(LICENSES_TEXT.into());
    let pkg_ver = env!("CARGO_PKG_VERSION");
    let commit = env!("ADULT_HUB_COMMIT_HASH");
    let build_date = env!("ADULT_HUB_BUILD_DATE");
    let ver_details = format!("v{pkg_ver} · Built {build_date} ({commit})");
    ui.set_app_version(pkg_ver.into());
    ui.set_app_version_details(ver_details.into());

    let player = match Player::new() {
        Ok(player) => Some(Arc::new(player)),
        Err(error) => {
            eprintln!("Player::new failed: {error}");
            show_error(&ui, error.to_string());
            None
        }
    };
    system_info::update_ui(&ui, player.as_deref());

    if let Some(player) = &player {
        if let Err(error) = player.set_volume(100.0) {
            eprintln!("cannot set initial volume: {error}");
        }
    }

    let db_path = default_db_path();
    let local_store = match LocalStore::open(&db_path) {
        Ok(store) => Arc::new(store),
        Err(err) => {
            eprintln!(
                "warning: opening local store at {db_path:?} failed ({err}), falling back to in-memory store"
            );
            Arc::new(LocalStore::open_in_memory().expect("in-memory sqlite store"))
        }
    };
    let _card_zoom_persistence = install_card_zoom_persistence(&ui, local_store.clone());
    let credential_store = Arc::new(CredentialStore::new());
    if let Ok(None) = credential_store.get()
        && let Ok(Some(legacy_session)) = local_store.load_legacy_session()
    {
        // Migrate the previous plaintext session to Windows Credential Manager.
        // If secure persistence is unavailable, keep it only for this run and
        // remove the old plaintext copy from SQLite.
        let _ = credential_store.save(legacy_session);
        let _ = local_store.clear_session();
    }
    let device_id = local_store
        .get_or_create_device_id()
        .unwrap_or_else(|_| "desktop-device-default".to_string());
    thumbnails::set_jellyfin_device_id(&device_id);
    let jellyfin = Arc::new(JellyfinClient::new(device_id));
    let resolver = Arc::new(ResolverClient::new());
    let reporter = Arc::new(PlaybackReporter::new(jellyfin.clone()));
    let session_ctrl = Arc::new(PlaybackSessionController::new(local_store.clone()));

    let session_opt = credential_store.get().ok().flatten();
    let saved_resolver_url = local_store
        .get_setting("resolver_base_url")
        .ok()
        .flatten()
        .or_else(|| local_store.get_setting("resolver_url").ok().flatten())
        .or_else(|| session_opt.as_ref().map(|s| s.server_url.clone()))
        .unwrap_or_default();
    let approved_http_origins = local_store.approved_cleartext_origins().unwrap_or_default();
    let session_origin_approved = session_opt.as_ref().is_none_or(|session| {
        match ServiceUrlPolicy::inspect(&session.server_url, false) {
            ServiceUrlValidation::Valid(url) => {
                !url.uses_cleartext || approved_http_origins.contains(&url.origin)
            }
            ServiceUrlValidation::Invalid(_) => false,
        }
    });
    let initial_resolver_url = match ServiceUrlPolicy::inspect(&saved_resolver_url, false) {
        ServiceUrlValidation::Valid(url)
            if !url.uses_cleartext || approved_http_origins.contains(&url.origin) =>
        {
            if !saved_resolver_url.is_empty() && saved_resolver_url != url.normalized {
                let _ = local_store.set_setting("resolver_base_url", &url.normalized);
                let _ = local_store.set_setting("resolver_url", &url.normalized);
            }
            url.normalized
        }
        _ => {
            if !saved_resolver_url.is_empty() {
                // Older settings may contain a LAN HTTP resolver URL saved
                // before explicit consent existed. Remove it before any
                // resolver worker starts.
                let _ = local_store.set_setting("resolver_base_url", "");
                let _ = local_store.set_setting("resolver_url", "");
            }
            String::new()
        }
    };
    let initial_auth_token = session_opt
        .as_ref()
        .filter(|_| session_origin_approved)
        .map(|s| s.access_token.clone());
    thumbnails::set_resolver_base_url(&initial_resolver_url);
    thumbnails::set_resolver_auth_token(initial_auth_token.as_deref());

    let phash_coord = phash::PhashCoordinator::new(
        local_store.clone(),
        resolver.clone(),
        initial_resolver_url.clone(),
        initial_auth_token,
    );

    let current_playing_item = Arc::new(Mutex::new(None));
    let cached_catalog = Arc::new(Mutex::new(Vec::new()));
    let autoplay_active = Arc::new(AtomicBool::new(false));
    let hwdec_active = Arc::new(AtomicBool::new(true));

    let ctx = AppContext {
        player: player.clone(),
        headers: options.headers.clone(),
        local_store: local_store.clone(),
        credential_store: credential_store.clone(),
        jellyfin: jellyfin.clone(),
        resolver: resolver.clone(),
        reporter: reporter.clone(),
        session_ctrl: session_ctrl.clone(),
        phash_coord: phash_coord.clone(),
        current_playing_item: current_playing_item.clone(),
        detail_active_item: Arc::new(Mutex::new(None)),
        pending_resume_position: Arc::new(Mutex::new(None)),
        cached_catalog: cached_catalog.clone(),
        home_catalog: Arc::new(Mutex::new(Vec::new())),
        performer_catalog: Arc::new(Mutex::new(Vec::new())),
        studio_catalog: Arc::new(Mutex::new(Vec::new())),
        autoplay_active: autoplay_active.clone(),
        hwdec_active: hwdec_active.clone(),
        player_paused_for_details: Arc::new(AtomicBool::new(false)),
        is_navigating_back: Arc::new(AtomicBool::new(false)),
        nav_history: Arc::new(Mutex::new(Vec::new())),
        shuffle_engine: Arc::new(crate::shuffle::ShuffleEngine::default()),
        download_manager: Arc::new(download::DownloadManager::new()),
        active_playback_context: Arc::new(Mutex::new(Vec::new())),
    };

    // Synchronize initial settings to UI
    if let Ok(config) = local_store.export_non_secret_config() {
        let limit_mb = config.cache_limit_mb;
        thumbnails::set_cache_limit_mb(limit_mb.max(0) as usize);
        thumbnails::set_low_ram_mode(config.low_ram_mode);
        autoplay_active.store(config.autoplay_next, std::sync::atomic::Ordering::SeqCst);
        ui.set_autoplay_active(config.autoplay_next);
        ui.set_seek_step_sec(config.seek_step_sec);
        ui.set_seek_step_label(format!("Step: {}s", config.seek_step_sec).into());
        let limit_str = if limit_mb == 0 {
            "Off (RAM only)".to_string()
        } else if limit_mb >= 1024 {
            format!("{} GB", limit_mb / 1024)
        } else {
            format!("{limit_mb} MB")
        };
        ui.set_cache_limit_label(format!("Cache Limit: {limit_str}").into());
        ui.set_cache_limit_mb(limit_mb);
        let used_bytes = thumbnails::current_cache_size_bytes();
        let used_str = thumbnails::format_cache_bytes(used_bytes);
        ui.set_cache_usage_text(format!("Used: {used_str} / Limit: {limit_str}").into());
        ui.set_phash_active(config.phash_on_playback);
        ui.set_low_ram_active(config.low_ram_mode);
        if let Some(url) = config
            .resolver_base_url
            .clone()
            .or_else(|| local_store.get_setting("resolver_url").ok().flatten())
        {
            ui.set_resolver_url_input(url.clone().into());
            ui.set_saved_resolver_url(url.into());
            ui.set_resolver_url_dirty(false);
        } else if !initial_resolver_url.is_empty() {
            ui.set_resolver_url_input(initial_resolver_url.clone().into());
            ui.set_saved_resolver_url(initial_resolver_url.clone().into());
            ui.set_resolver_url_dirty(false);
        }
        if let Some(lib_name) = config.selected_library_name {
            ui.set_current_library(lib_name.into());
        }
        let tags_model = Rc::new(slint::VecModel::from(
            config
                .excluded_tags
                .into_iter()
                .map(slint::SharedString::from)
                .collect::<Vec<_>>(),
        ));
        ui.set_excluded_tags_list(tags_model.into());
        let channels_model = Rc::new(slint::VecModel::from(
            config
                .hidden_channel_ids
                .into_iter()
                .map(slint::SharedString::from)
                .collect::<Vec<_>>(),
        ));
        ui.set_hidden_channels_list(channels_model.into());
    }
    handlers::refresh_approved_http_origins(&ui, &local_store);

    let frame_pending = Arc::new(AtomicBool::new(false));
    let _polling_timer = handlers::install_all_handlers(&ui, &ctx, frame_pending.clone());

    {
        let weak = ui.as_weak();
        let credential_store = credential_store.clone();
        ui.on_retry_thumbnail(move |item| {
            let Some(handle) = weak.upgrade() else { return };
            let active_screen = handle.get_active_screen();
            let request = catalog::ThumbnailRequest::from(&item);
            let token = credential_store
                .get()
                .ok()
                .flatten()
                .map(|session| session.access_token);
            if active_screen == "home" {
                catalog::retry_home_thumbnail_async(weak.clone(), request, token);
                return;
            }
            let channel = active_screen == "channels";
            catalog::retry_thumbnail_async(weak.clone(), request, token, channel);
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_reflow_grid(move |catalog_columns, channel_columns| {
            let Some(ui) = weak.upgrade() else { return };
            catalog::reflow_grid(
                &ui,
                catalog_columns.max(1) as usize,
                channel_columns.max(1) as usize,
            );
        });
    }

    // Persist playback position and stop the Jellyfin session when the user
    // closes the desktop window with the native title-bar control.
    {
        let weak = ui.as_weak();
        let player = player.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_playing_item.clone();
        ui.window().on_close_requested(move || {
            if let Some(ui) = weak.upgrade()
                && ui.get_active_screen() == "player"
            {
                playback::stop_active_playback_for_navigation(
                    &ui,
                    player.as_deref(),
                    &reporter,
                    &session_ctrl,
                    &current_item,
                );
            }
            slint::CloseRequestResponse::HideWindow
        });
    }

    // Restore an HTTP session only after explicit consent for its origin.
    let pending_http_restore = Arc::new(Mutex::new(None::<Session>));
    if let Ok(Some(session)) = credential_store.get() {
        if session_origin_approved {
            activate_session(
                &ui,
                &session,
                &reporter,
                &phash_coord,
                &initial_resolver_url,
            );
        } else {
            *pending_http_restore.lock().unwrap() = Some(session.clone());
            if let ServiceUrlValidation::Valid(url) =
                ServiceUrlPolicy::inspect(&session.server_url, false)
            {
                ui.set_cleartext_consent_url(url.normalized.into());
                ui.set_cleartext_consent_action("restore-session".into());
                ui.set_cleartext_consent_visible(true);
            }
            ui.set_server_url_input(session.server_url.into());
            ui.set_logged_in_user(session.username.into());
        }
    } else {
        set_catalog(&ui, Vec::new());
    }

    // Background server discovery on startup
    {
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let servers = discover_servers(Duration::from_millis(2000));
            if !servers.is_empty() {
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    let items = servers
                        .iter()
                        .map(|s| DiscoveredServerItem {
                            id: s.id.clone().into(),
                            name: s.name.clone().into(),
                            url: s.url.clone().into(),
                        })
                        .collect::<Vec<_>>();
                    set_discovered_servers(&ui, items);
                })
                .ok();
            }
        });
    }

    // Register Toast and Modal Callbacks
    {
        let weak = ui.as_weak();
        ui.on_open_resolver_settings(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_resolver_prompt_dialog_visible(false);
            ui.set_toast_visible(false);
            ui.set_active_screen("settings".into());
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_close_resolver_prompt(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_resolver_prompt_dialog_visible(false);
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_close_login_modal(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_login_modal_visible(false);
        });
    }

    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let pending_http_restore = pending_http_restore.clone();
        let reporter = reporter.clone();
        let phash_coord = phash_coord.clone();
        let initial_resolver_url = initial_resolver_url.clone();
        ui.on_accept_cleartext_http_consent(move || {
            let Some(ui) = weak.upgrade() else { return };
            let url = ui.get_cleartext_consent_url().to_string();
            let service_url = match ServiceUrlPolicy::inspect(&url, false) {
                ServiceUrlValidation::Valid(url) if url.uses_cleartext => url,
                ServiceUrlValidation::Valid(_) => {
                    ui.set_cleartext_consent_visible(false);
                    show_error(&ui, "Consent is only required for local HTTP servers");
                    return;
                }
                ServiceUrlValidation::Invalid(error) => {
                    ui.set_cleartext_consent_visible(false);
                    show_error(&ui, error);
                    return;
                }
            };
            if let Err(error) = local_store.approve_cleartext_origin(&service_url.origin) {
                show_error(&ui, format!("Could not save HTTP consent: {error}"));
                return;
            }

            let action = ui.get_cleartext_consent_action().to_string();
            ui.set_cleartext_consent_visible(false);
            ui.set_cleartext_consent_action("".into());
            handlers::refresh_approved_http_origins(&ui, &local_store);

            match action.as_str() {
                "login" => ui.invoke_perform_login(
                    ui.get_server_url_input(),
                    ui.get_username_input(),
                    ui.get_password_input(),
                ),
                "quick-connect" => ui.invoke_start_quick_connect(),
                "discover-server" => ui.invoke_select_discovered_server(ui.get_server_url_input()),
                "resolver-save" => ui.invoke_save_resolver_url(ui.get_resolver_url_input()),
                "resolver-test" => ui.invoke_test_resolver_connection(),
                "check-connections" => ui.invoke_check_connections(),
                "restore-session" => {
                    if let Some(session) = pending_http_restore.lock().unwrap().take() {
                        let resolver_url = if initial_resolver_url.is_empty() {
                            session.server_url.clone()
                        } else {
                            initial_resolver_url.clone()
                        };
                        activate_session(&ui, &session, &reporter, &phash_coord, &resolver_url);
                    }
                }
                _ => {}
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_dismiss_cleartext_http_consent(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_cleartext_consent_visible(false);
            ui.set_cleartext_consent_action("".into());
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_toast_action_clicked(move || {
            let Some(ui) = weak.upgrade() else { return };
            let action = ui.get_toast_action_text().to_string();
            ui.set_toast_visible(false);
            if action == "Configure" {
                ui.set_resolver_prompt_dialog_visible(false);
                ui.set_active_screen("settings".into());
            } else if action == "Sign In" {
                ui.set_login_modal_visible(true);
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_toast_dismiss(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_toast_visible(false);
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_activate_focused_card(move || {
            let Some(ui) = weak.upgrade() else { return };
            let idx = ui.get_focused_card_index() as usize;
            let items = ui.get_catalog_items();
            if let Some(card) = items.row_data(idx) {
                ui.invoke_play_item(card.id);
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_activate_focused_channel_card(move || {
            let Some(ui) = weak.upgrade() else { return };
            let idx = ui.get_focused_channel_card_index() as usize;
            let items = ui.get_channel_items();
            if let Some(card) = items.row_data(idx) {
                ui.invoke_play_item(card.id);
            }
        });
    }

    if let Some(ref p) = player {
        let on_renderer_ready: Box<dyn FnOnce()> = if let Some(initial) = initial_media(&options) {
            let player = p.clone();
            let headers = options.headers.clone();
            let open_player = options.url.is_some();
            let ui_weak = ui.as_weak();
            let credential_store = credential_store.clone();
            let local_store = local_store.clone();
            let jellyfin = jellyfin.clone();
            let reporter = reporter.clone();
            let session_ctrl = session_ctrl.clone();
            let current_playing_item = current_playing_item.clone();
            let phash_coord = phash_coord.clone();
            Box::new(move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                eprintln!("on_renderer_ready invoked: initial={initial}");
                if initial == "sample" {
                    let sample_item = sample_catalog_tv_items()[0].clone();
                    start_item_playback(
                        &ui,
                        &player,
                        sample_item,
                        &headers,
                        &credential_store,
                        &local_store,
                        &jellyfin,
                        &reporter,
                        &session_ctrl,
                        &current_playing_item,
                        &phash_coord,
                    );
                } else {
                    load_media(&ui, &player, &initial, &headers);
                    ui.set_url_text(initial.into());
                }
                if open_player {
                    ui.set_active_screen("player".into());
                }
            })
        } else {
            Box::new(|| {})
        };
        install_renderer(&ui, p.clone(), &frame_pending, on_renderer_ready)?;
    } else {
        ui.set_status("Ready.".into());
    }

    if options.demo_shuffle {
        run_demo_shuffle(&ui);
    }

    ui.show()?;
    ui.run()?;
    let _ = local_store.set_setting("card_zoom", &format!("{:.4}", ui.get_card_zoom()));
    Ok(())
}

fn install_card_zoom_persistence(
    ui: &MainWindow,
    local_store: Arc<LocalStore>,
) -> Rc<slint::Timer> {
    const KEY: &str = "card_zoom";

    let initial_zoom = local_store
        .get_setting(KEY)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.6, 1.6))
        .unwrap_or(1.0);
    ui.set_card_zoom(initial_zoom);

    let pending_zoom = Rc::new(Cell::new(initial_zoom));
    let timer = Rc::new(slint::Timer::default());
    let save_value = pending_zoom.clone();
    timer.start(
        slint::TimerMode::SingleShot,
        Duration::from_millis(350),
        move || {
            let _ = local_store.set_setting(KEY, &format!("{:.4}", save_value.get()));
        },
    );

    let restart_timer = timer.clone();
    ui.on_card_zoom_changed(move |value| {
        pending_zoom.set(value.clamp(0.6, 1.6));
        restart_timer.restart();
    });
    timer
}

/// Handle card zoom before Slint dispatches input. All unrelated pointer and
/// keyboard events stay on Slint's normal path.
fn install_card_zoom_event_filter(ui: &MainWindow) {
    use slint::ComponentHandle;

    let weak = ui.as_weak();
    // Keep Winit's modifier events and physical Ctrl key transitions, with
    // Windows key-state checks as a fallback for wheel messages that arrive
    // between keyboard and modifier events.
    let mut ctrl_modifier = false;
    let mut left_ctrl_down = false;
    let mut right_ctrl_down = false;
    ui.window()
        .on_winit_window_event(move |slint_window, event| {
            match event {
                winit::event::WindowEvent::ModifiersChanged(modifiers) => {
                    ctrl_modifier = modifiers.state().control_key();
                }
                winit::event::WindowEvent::KeyboardInput { event: key, .. } => {
                    use winit::keyboard::{KeyCode, PhysicalKey};
                    let is_down = key.state == winit::event::ElementState::Pressed;
                    match key.physical_key {
                        PhysicalKey::Code(KeyCode::ControlLeft) => left_ctrl_down = is_down,
                        PhysicalKey::Code(KeyCode::ControlRight) => right_ctrl_down = is_down,
                        _ => {}
                    }
                }
                winit::event::WindowEvent::Focused(false) => {
                    ctrl_modifier = false;
                    left_ctrl_down = false;
                    right_ctrl_down = false;
                }
                _ => {}
            }

            #[cfg(target_os = "windows")]
            let ctrl_held = {
                use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
                    GetAsyncKeyState, GetKeyState, VK_CONTROL,
                };
                ctrl_modifier
                    || left_ctrl_down
                    || right_ctrl_down
                    || (unsafe { GetKeyState(VK_CONTROL as i32) }) as u16 & 0x8000 != 0
                    || (unsafe { GetAsyncKeyState(VK_CONTROL as i32) }) as u16 & 0x8000 != 0
            };

            #[cfg(not(target_os = "windows"))]
            let ctrl_held = ctrl_modifier || left_ctrl_down || right_ctrl_down;

            let Some(ui) = weak.upgrade() else {
                return EventResult::Propagate;
            };
            let screen = ui.get_active_screen();
            let browse_surface = matches!(
                screen.as_str(),
                "browse" | "channels" | "performer-details" | "studio-details"
            );
            let modal_open = ui.get_is_fullscreen()
                || ui.get_catalog_filter_picker_visible()
                || ui.get_browse_subnav_menu_open()
                || ui.get_channel_picker_open()
                || ui.get_cleartext_consent_visible()
                || ui.get_licenses_visible()
                || ui.get_download_panel_open()
                || ui.get_resolver_prompt_dialog_visible()
                || ui.get_login_modal_visible()
                || ui.get_confirm_dialog_visible();
            if !browse_surface || modal_open {
                return EventResult::Propagate;
            }

            let text_input_focused = ui.get_text_input_focused();
            if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event {
                if ctrl_held
                    && key.state == winit::event::ElementState::Pressed
                    && !key.repeat
                    && !text_input_focused
                {
                    use winit::keyboard::{Key, KeyCode, PhysicalKey};
                    let key_action = match key.physical_key {
                        PhysicalKey::Code(KeyCode::Equal | KeyCode::NumpadAdd) => Some(1.0_f32),
                        PhysicalKey::Code(KeyCode::Minus | KeyCode::NumpadSubtract) => {
                            Some(-1.0_f32)
                        }
                        PhysicalKey::Code(KeyCode::Digit0 | KeyCode::Numpad0) => Some(0.0_f32),
                        _ => match key.logical_key.as_ref() {
                            Key::Character("+" | "=") => Some(1.0),
                            Key::Character("-") => Some(-1.0),
                            Key::Character("0") => Some(0.0),
                            _ => None,
                        },
                    };
                    if let Some(action) = key_action {
                        let zoom = if action == 0.0 {
                            1.0
                        } else {
                            (ui.get_card_zoom() + action * 0.1).clamp(0.6, 1.6)
                        };
                        ui.set_card_zoom(zoom);
                        return EventResult::PreventDefault;
                    }
                }
                return EventResult::Propagate;
            }

            let winit::event::WindowEvent::MouseWheel { delta, .. } = event else {
                return EventResult::Propagate;
            };
            if !ctrl_held || text_input_focused {
                return EventResult::Propagate;
            }

            let delta_y = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                winit::event::MouseScrollDelta::PixelDelta(position) => {
                    let scale = slint_window
                        .with_winit_window(|window| window.scale_factor())
                        .unwrap_or(1.0);
                    position.y / scale / 60.0
                }
            };
            if delta_y == 0.0 {
                return EventResult::Propagate;
            }

            // Winit reports wheel lines or high-resolution pixel deltas. Scale
            // linearly so touchpad movement remains smooth and small deltas count.
            let zoom = (f64::from(ui.get_card_zoom()) + delta_y * 0.1).clamp(0.6, 1.6);
            ui.set_card_zoom(zoom as f32);
            EventResult::PreventDefault
        });
}

fn run_demo_shuffle(ui: &MainWindow) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        use std::fs;
        use std::thread::sleep;
        use std::time::Duration;

        // Step 1: Let the initial video (sample 1) play with active video and audio
        sleep(Duration::from_millis(2200));
        let _ = fs::write("/tmp/ah_demo_shuffle_step_1", "playing sample 1");

        // Step 2: Trigger Shuffle via the official Slint callback (same as clicking Shuffle)
        let _ = weak.upgrade_in_event_loop(|ui| {
            ui.invoke_play_random();
        });

        // Step 3: Resolving state with animated spinner and cut-off audio
        sleep(Duration::from_millis(700));
        let _ = fs::write("/tmp/ah_demo_shuffle_step_2", "resolving");

        // Step 4: Next video (sample 2 with 880Hz audio) loads and starts playing automatically
        sleep(Duration::from_millis(2600));
        let _ = fs::write("/tmp/ah_demo_shuffle_step_3", "playing sample 2");

        // Step 5: Completed
        let _ = fs::write("/tmp/ah_demo_shuffle_done", "done");
        sleep(Duration::from_millis(400));
        std::process::exit(0);
    });
}

fn set_window_icon(ui: &MainWindow) {
    let Ok(image) =
        image::load_from_memory(include_bytes!("../../../assets/adult-hub-desktop.ico"))
    else {
        return;
    };
    let image = image
        .resize_exact(64, 64, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    let (width, height) = image.dimensions();
    let Ok(icon) =
        slint::winit_030::winit::window::Icon::from_rgba(image.into_raw(), width, height)
    else {
        return;
    };
    ui.window()
        .with_winit_window(|window| window.set_window_icon(Some(icon)));
}

fn activate_session(
    ui: &MainWindow,
    session: &Session,
    reporter: &PlaybackReporter,
    phash_coord: &phash::PhashCoordinator,
    resolver_url: &str,
) {
    reporter.set_auth(
        Some(session.server_url.clone()),
        Some(session.access_token.clone()),
    );
    let effective_resolver_url = if resolver_url.is_empty() {
        session.server_url.as_str()
    } else {
        resolver_url
    };
    crate::thumbnails::set_resolver_base_url(effective_resolver_url);
    crate::thumbnails::set_resolver_auth_token(Some(&session.access_token));
    phash_coord.update_auth(effective_resolver_url, Some(&session.access_token));
    ui.set_is_logged_in(true);
    ui.set_logged_in_user(session.username.clone().into());
    ui.set_server_url_input(session.server_url.clone().into());
    ui.invoke_filter_category("Latest".into());
    ui.invoke_refresh_library_list();
    ui.invoke_refresh_home();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::sample_catalog_items;
    use adult_hub_client::TvItem;
    use catalog::*;
    use options::Options;
    use playback::*;

    #[test]
    fn options_parse_empty() {
        let opts = Options::parse(Vec::<String>::new()).unwrap();
        assert!(opts.url.is_none());
        assert!(opts.headers.is_empty());
    }

    #[test]
    fn options_parse_url() {
        let opts = Options::parse(["http://example.com/video.mp4".to_string()]).unwrap();
        assert_eq!(opts.url.as_deref(), Some("http://example.com/video.mp4"));
        assert!(opts.headers.is_empty());
    }

    #[test]
    fn options_parse_headers() {
        let opts = Options::parse([
            "--header".to_string(),
            "Authorization: Bearer secret".to_string(),
            "--header=User-Agent: Desktop/1.0".to_string(),
            "sample.mp4".to_string(),
        ])
        .unwrap();
        assert_eq!(opts.url.as_deref(), Some("sample.mp4"));
        assert_eq!(
            opts.headers,
            vec![
                "Authorization: Bearer secret".to_string(),
                "User-Agent: Desktop/1.0".to_string(),
            ]
        );
    }

    #[test]
    fn options_parse_errors() {
        assert!(Options::parse(["--header".to_string()]).is_err());
        assert!(Options::parse(["--unknown-flag".to_string()]).is_err());
        assert!(Options::parse(["url1".to_string(), "url2".to_string()]).is_err());
    }

    #[test]
    fn format_time_various_durations() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(-10.0), "0:00");
        assert_eq!(format_time(f64::NAN), "0:00");
        assert_eq!(format_time(f64::INFINITY), "0:00");
        assert_eq!(format_time(9.0), "0:09");
        assert_eq!(format_time(65.0), "1:05");
        assert_eq!(format_time(599.0), "9:59");
        assert_eq!(format_time(3600.0), "1:00:00");
        assert_eq!(format_time(3665.0), "1:01:05");
    }

    #[test]
    fn resolve_media_input_urls() {
        assert_eq!(
            resolve_media_input("https://server.local/media/1.mp4"),
            "https://server.local/media/1.mp4"
        );
        assert_eq!(
            resolve_media_input("http://server.local/stream.m3u8"),
            "http://server.local/stream.m3u8"
        );
        assert_eq!(
            resolve_media_input("file:///var/media/sample.mp4"),
            "file:///var/media/sample.mp4"
        );
        assert_eq!(
            resolve_media_input("/opt/media/video.mp4"),
            "/opt/media/video.mp4"
        );
    }

    #[test]
    fn tv_item_to_card_mapping() {
        let item = TvItem {
            id: "jellyfin-123".into(),
            title: "Adult Scene #1".into(),
            overview: None,
            year: Some(2025),
            runtime_ticks: Some(120_000_000_000), // 12000s = 200m = 3h 20m
            genres: vec![],
            tags: vec![],
            studios: vec!["Lust Cinema".into()],
            people: vec![make_sample_person("Eva Green")],
            primary_image_tag: None,
            backdrop_image_tag: None,
            thumb_image_tag: None,
            child_count: None,
            is_favorite: true,
            playback_position_ticks: 0,
            played_percentage: None,
            is_played: false,
            provider_video_id: None,
            source_provider: None,
            media_source_id: None,
            container: None,
            file_size_bytes: None,
            is_placeholder_source: false,
            width: None,
            height: None,
            community_rating: None,
        };

        let card = tv_item_to_card(&item);
        assert_eq!(card.id, "jellyfin-123");
        assert_eq!(card.title, "Adult Scene #1");
        assert_eq!(card.year, "2025");
        assert_eq!(card.duration, "3h 20m");
        assert_eq!(card.provider, "jellyfin");
        assert_eq!(card.thumbnail_state, "missing");
        assert!(!card.is_played);
        assert!(card.is_favorite);
        assert_eq!(card.studio, "Lust Cinema");
    }

    #[test]
    fn stage7_similar_items_ranking() {
        let target = TvItem {
            id: "target-1".into(),
            title: "Target Video".into(),
            overview: None,
            year: Some(2025),
            runtime_ticks: Some(120_000_000_000),
            genres: vec!["Blonde".into(), "Hardcore".into()],
            tags: vec![],
            studios: vec!["Brazzers".into()],
            people: vec![make_sample_person("Eva Elfie")],
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
            is_placeholder_source: false,
            width: None,
            height: None,
            community_rating: None,
        };

        let item_high_match = TvItem {
            id: "high-match".into(),
            title: "High Match Video".into(),
            overview: None,
            year: Some(2025),
            runtime_ticks: None,
            genres: vec!["Blonde".into()],
            tags: vec![],
            studios: vec!["Brazzers".into()],
            people: vec![make_sample_person("Eva Elfie")],
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
            is_placeholder_source: false,
            width: None,
            height: None,
            community_rating: None,
        };

        let item_low_match = TvItem {
            id: "low-match".into(),
            title: "Low Match Video".into(),
            overview: None,
            year: Some(2025),
            runtime_ticks: None,
            genres: vec!["Hardcore".into()],
            tags: vec![],
            studios: vec!["Other Studio".into()],
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
            is_placeholder_source: false,
            width: None,
            height: None,
            community_rating: None,
        };

        let item_no_match = TvItem {
            id: "no-match".into(),
            title: "No Match Video".into(),
            overview: None,
            year: Some(2025),
            runtime_ticks: None,
            genres: vec!["Brunette".into()],
            tags: vec![],
            studios: vec!["Different Studio".into()],
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
            is_placeholder_source: false,
            width: None,
            height: None,
            community_rating: None,
        };

        let catalog = vec![
            target.clone(),
            item_no_match,
            item_low_match.clone(),
            item_high_match.clone(),
        ];

        let mut scored: Vec<(i32, TvItem)> = catalog
            .into_iter()
            .filter(|i| i.id != target.id)
            .map(|i| {
                let mut score = 0;
                for g in &target.genres {
                    if i.genres.iter().any(|ig| ig.eq_ignore_ascii_case(g)) {
                        score += 3;
                    }
                }
                for s in &target.studios {
                    if i.studios.iter().any(|is| is.eq_ignore_ascii_case(s)) {
                        score += 5;
                    }
                }
                for p in &target.people {
                    if i.people
                        .iter()
                        .any(|ip| ip.name.eq_ignore_ascii_case(&p.name))
                    {
                        score += 4;
                    }
                }
                (score, i)
            })
            .collect();

        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        let similar: Vec<TvItem> = scored
            .into_iter()
            .filter(|(s, _)| *s > 0)
            .map(|(_, i)| i)
            .collect();

        assert_eq!(similar.len(), 2);
        assert_eq!(similar[0].id, "high-match");
        assert_eq!(similar[1].id, "low-match");
    }

    #[test]
    fn stage8_channel_category_and_search_filtering() {
        let v1 = TvItem {
            id: "telegram:101".into(),
            title: "4K UHD Exclusive Scene".into(),
            year: Some(2026),
            runtime_ticks: Some(30 * 60 * 10_000_000), // 30m
            genres: vec!["Exclusive".into()],
            studios: vec!["Private Studio".into()],
            people: vec![make_sample_person("Angela White")],
            width: Some(3840),
            height: Some(2160),
            ..Default::default()
        };

        let v2 = TvItem {
            id: "telegram:102".into(),
            title: "VR Experience 180 3D".into(),
            year: Some(2026),
            runtime_ticks: Some(10 * 60 * 10_000_000), // 10m (highlight clip)
            genres: vec!["VR".into()],
            people: vec![make_sample_person("Maitland Ward")],
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        };

        let items = [v1.clone(), v2.clone()];

        // 4K filter test
        let k4_filtered: Vec<_> = items
            .iter()
            .filter(|i| {
                i.height.is_some_and(|h| h >= 2160) || i.title.to_lowercase().contains("4k")
            })
            .collect();
        assert_eq!(k4_filtered.len(), 1);
        assert_eq!(k4_filtered[0].id, "telegram:101");

        // VR filter test
        let vr_filtered: Vec<_> = items
            .iter()
            .filter(|i| {
                i.genres.iter().any(|g| g.to_lowercase().contains("vr"))
                    || i.title.to_lowercase().contains("vr")
            })
            .collect();
        assert_eq!(vr_filtered.len(), 1);
        assert_eq!(vr_filtered[0].id, "telegram:102");

        // Highlight clip (< 15m)
        let highlight_filtered: Vec<_> = items
            .iter()
            .filter(|i| i.runtime_ticks.is_some_and(|t| t < 15 * 60 * 10_000_000))
            .collect();
        assert_eq!(highlight_filtered.len(), 1);
        assert_eq!(highlight_filtered[0].id, "telegram:102");

        // Search by performer in channel
        let search_angela: Vec<_> = items
            .iter()
            .filter(|i| {
                i.people
                    .iter()
                    .any(|p| p.name.to_lowercase().contains("angela"))
            })
            .collect();
        assert_eq!(search_angela.len(), 1);
        assert_eq!(search_angela[0].id, "telegram:101");
    }

    #[test]
    fn sample_catalog_defaults() {
        let samples = sample_catalog_items();
        assert!(!samples.is_empty());
        assert_eq!(samples[0].id, "sample");
        assert_eq!(samples[0].provider, "local");

        let channels = sample_channel_items();
        assert!(!channels.is_empty());
        assert!(channels[0].id.starts_with("telegram:"));
    }

    #[test]
    fn stage9_volume_clamping_and_step_adjustment() {
        let clamp_vol = |cur: f32, delta: f32| -> f32 { (cur + delta).clamp(0.0, 100.0) };

        assert_eq!(clamp_vol(98.0, 5.0), 100.0);
        assert_eq!(clamp_vol(3.0, -5.0), 0.0);
        assert_eq!(clamp_vol(50.0, 5.0), 55.0);
        assert_eq!(clamp_vol(50.0, -5.0), 45.0);
    }

    #[test]
    fn test_cards_to_rows_partitioning() {
        use crate::catalog::cards_to_rows;
        let cards = sample_catalog_items();
        assert_eq!(cards.len(), 4);

        // 4 cols per row
        let rows_4 = cards_to_rows(&cards, 4);
        assert_eq!(rows_4.len(), 1);
        assert!(rows_4[0].has_0);
        assert!(rows_4[0].has_1);
        assert!(rows_4[0].has_2);
        assert!(rows_4[0].has_3);
        assert_eq!(rows_4[0].card_0.id, "sample");

        // 2 cols per row
        let rows_2 = cards_to_rows(&cards, 2);
        assert_eq!(rows_2.len(), 2);
        assert!(rows_2[0].has_0);
        assert!(rows_2[0].has_1);
        assert!(!rows_2[0].has_2);
        assert!(rows_2[1].has_0);
        assert!(rows_2[1].has_1);
    }

    #[test]
    fn partial_catalog_rows_keep_their_fixed_column_slots() {
        use crate::catalog::cards_to_rows;

        let cards_for = |count: usize| {
            (0..count)
                .map(|index| VideoCardData {
                    id: format!("layout-{index}").into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>()
        };

        assert!(cards_to_rows(&cards_for(0), 5).is_empty());
        for count in 1..=3 {
            let rows = cards_to_rows(&cards_for(count), 5);
            assert_eq!(rows.len(), 1);
            let occupied = [
                rows[0].has_0,
                rows[0].has_1,
                rows[0].has_2,
                rows[0].has_3,
                rows[0].has_4,
            ];
            assert!(occupied.iter().take(count).all(|slot| *slot));
            assert!(occupied.iter().skip(count).all(|slot| !slot));
        }

        let full_and_partial = cards_to_rows(&cards_for(7), 5);
        assert_eq!(full_and_partial.len(), 2);
        assert_eq!(
            [
                full_and_partial[0].has_0,
                full_and_partial[0].has_1,
                full_and_partial[0].has_2,
                full_and_partial[0].has_3,
                full_and_partial[0].has_4,
            ],
            [true; 5]
        );
        assert_eq!(
            [
                full_and_partial[1].has_0,
                full_and_partial[1].has_1,
                full_and_partial[1].has_2,
                full_and_partial[1].has_3,
                full_and_partial[1].has_4,
            ],
            [true, true, false, false, false]
        );
    }

    #[test]
    fn test_sample_media_resolution_fallback() {
        use crate::options::{PUBLIC_SAMPLE_MP4_URL, resolve_sample_media};
        let resolved = resolve_sample_media();
        assert!(!resolved.is_empty());
        if !std::path::Path::new("sample.mp4").is_file() {
            assert_eq!(resolved, PUBLIC_SAMPLE_MP4_URL);
        }
    }

    #[test]
    fn test_telegram_resolver_missing_error() {
        use adult_hub_client::TvItem;
        let item = TvItem {
            id: "telegram:chan:123".into(),
            title: "Telegram test".into(),
            ..Default::default()
        };
        let err =
            PlaybackSessionController::resolve_stream_url(&item, None, None, None).unwrap_err();
        assert!(err.contains("Resolver URL is required"));
    }

    #[test]
    fn test_find_catalog_item_from_home_or_history_not_in_browse() {
        let local_store = Arc::new(adult_hub_storage::LocalStore::open_in_memory().unwrap());
        let cred_store = Arc::new(adult_hub_storage::CredentialStore::new());
        let session_ctrl = Arc::new(PlaybackSessionController::new(local_store.clone()));
        let jellyfin = Arc::new(adult_hub_client::JellyfinClient::new("test-device"));
        let resolver = Arc::new(adult_hub_client::ResolverClient::new());
        let reporter = Arc::new(crate::reporter::PlaybackReporter::new(jellyfin.clone()));
        let phash_coord = crate::phash::PhashCoordinator::new(
            local_store.clone(),
            resolver.clone(),
            "http://localhost:8080".into(),
            None,
        );

        let home_item = TvItem {
            id: "home-video-123".into(),
            title: "Home Featured Video".into(),
            ..Default::default()
        };
        let history_item = TvItem {
            id: "history-video-456".into(),
            title: "Watched History Video".into(),
            ..Default::default()
        };
        let detail_item = TvItem {
            id: "detail-active-789".into(),
            title: "Detail Active Video".into(),
            ..Default::default()
        };

        // Record history item in SQLite
        local_store
            .record_watch_history(
                &history_item.id,
                &serde_json::to_string(&history_item).unwrap(),
            )
            .unwrap();

        let ctx = AppContext {
            player: None,
            headers: Vec::new(),
            local_store,
            credential_store: cred_store,
            jellyfin,
            resolver,
            reporter,
            session_ctrl,
            phash_coord,
            current_playing_item: Arc::new(Mutex::new(None)),
            detail_active_item: Arc::new(Mutex::new(Some(detail_item.clone()))),
            pending_resume_position: Arc::new(Mutex::new(None)),
            cached_catalog: Arc::new(Mutex::new(vec![])), // Browse is empty!
            home_catalog: Arc::new(Mutex::new(vec![home_item.clone()])),
            performer_catalog: Arc::new(Mutex::new(vec![])),
            studio_catalog: Arc::new(Mutex::new(vec![])),
            autoplay_active: Arc::new(AtomicBool::new(false)),
            hwdec_active: Arc::new(AtomicBool::new(false)),
            player_paused_for_details: Arc::new(AtomicBool::new(false)),
            is_navigating_back: Arc::new(AtomicBool::new(false)),
            nav_history: Arc::new(Mutex::new(Vec::new())),
            shuffle_engine: Arc::new(crate::shuffle::ShuffleEngine::default()),
            download_manager: Arc::new(download::DownloadManager::new()),
            active_playback_context: Arc::new(Mutex::new(Vec::new())),
        };

        // 1. Home item should be found even when Browse cached_catalog is empty
        let found_home = ctx.find_catalog_item("home-video-123");
        assert!(found_home.is_some());
        assert_eq!(found_home.unwrap().title, "Home Featured Video");

        // 2. History item should be found even when Browse and Home are empty
        let found_history = ctx.find_catalog_item("history-video-456");
        assert!(found_history.is_some());
        assert_eq!(found_history.unwrap().title, "Watched History Video");

        // 3. Detail active item should be found
        let found_detail = ctx.find_catalog_item("detail-active-789");
        assert!(found_detail.is_some());
        assert_eq!(found_detail.unwrap().title, "Detail Active Video");

        // 4. Non-existent item returns None
        assert!(ctx.find_catalog_item("non-existent-999").is_none());
    }

    #[test]
    fn test_cards_to_rows_8_columns() {
        use crate::catalog::cards_to_rows;
        let mut cards = Vec::new();
        for i in 0..10 {
            cards.push(VideoCardData {
                id: format!("card_{i}").into(),
                title: format!("Title {i}").into(),
                ..Default::default()
            });
        }

        let rows_8 = cards_to_rows(&cards, 8);
        assert_eq!(rows_8.len(), 2);
        // First row has 8 cards
        assert!(rows_8[0].has_0);
        assert!(rows_8[0].has_1);
        assert!(rows_8[0].has_2);
        assert!(rows_8[0].has_3);
        assert!(rows_8[0].has_4);
        assert!(rows_8[0].has_5);
        assert!(rows_8[0].has_6);
        assert!(rows_8[0].has_7);
        assert_eq!(rows_8[0].card_0.id, "card_0");
        assert_eq!(rows_8[0].card_7.id, "card_7");

        // Second row has 2 cards, remaining 6 are false
        assert!(rows_8[1].has_0);
        assert!(rows_8[1].has_1);
        assert!(!rows_8[1].has_2);
        assert!(!rows_8[1].has_7);
        assert_eq!(rows_8[1].card_0.id, "card_8");
        assert_eq!(rows_8[1].card_1.id, "card_9");
    }

    #[test]
    fn test_resume_position_safeguard() {
        let local_store = Arc::new(adult_hub_storage::LocalStore::open_in_memory().unwrap());
        let session_ctrl = PlaybackSessionController::new(local_store.clone());

        let item = TvItem {
            id: "telegram:chan:456".into(),
            provider_video_id: Some("456".into()),
            source_provider: Some("telegram".into()),
            title: "Resume Test".into(),
            ..Default::default()
        };

        // Position 3s (< 5s threshold) should not overwrite or be saved
        session_ctrl.checkpoint_progress(&item, 3_000, 100_000, false);
        assert_eq!(session_ctrl.get_telegram_resume_position("456"), None);

        // Position 23s should be saved
        session_ctrl.checkpoint_progress(&item, 23_000, 100_000, false);
        assert_eq!(
            session_ctrl.get_telegram_resume_position("456"),
            Some(23_000)
        );

        // When starting new item, pending_resume_position is correctly applied
        let pending = Arc::new(Mutex::new(Some(23.0f64)));
        let resume_target = pending.lock().unwrap().take();
        assert_eq!(resume_target, Some(23.0));
        assert_eq!(*pending.lock().unwrap(), None);
    }

    #[test]
    fn test_auto_resume_logic_without_prompt() {
        let duration_ms: u64 = 600_000; // 10 minutes

        // 1. Position < 10s should start from 0:00 (no resume)
        assert!(!PlaybackSessionController::should_prompt_resume(
            5_000,
            duration_ms
        ));

        // 2. Position midway (e.g. 120s) should automatically resume
        assert!(PlaybackSessionController::should_prompt_resume(
            120_000,
            duration_ms
        ));

        // 3. Completed item (> 95% or within 30s of end) should restart at 0:00
        assert!(!PlaybackSessionController::should_prompt_resume(
            585_000,
            duration_ms
        )); // within 15s of end
        assert!(!PlaybackSessionController::should_prompt_resume(
            580_000,
            duration_ms
        )); // 96.6% (> 95%)

        // 4. Start pos calculation:
        // Valid progress auto-resumes to recorded position
        let saved_pos_ms = 120_000u64;
        let auto_start_secs =
            if PlaybackSessionController::should_prompt_resume(saved_pos_ms, duration_ms) {
                Some(saved_pos_ms as f64 / 1000.0)
            } else {
                None
            };
        assert_eq!(auto_start_secs, Some(120.0));

        // Explicit "Play from Start" forces None (0:00)
        let forced_from_start = Some(false);
        let start_pos_secs = match forced_from_start {
            Some(false) => None,
            Some(true) => auto_start_secs,
            None => auto_start_secs,
        };
        assert_eq!(start_pos_secs, None);
    }

    #[test]
    fn test_home_51_percent_resume_discrepancy_resolved() {
        // Video of 23 minutes 38 seconds = 1,418 seconds = 1,418,000 ms
        let duration_ms: u64 = 1_418_000;
        let local_store = Arc::new(adult_hub_storage::LocalStore::open_in_memory().unwrap());
        let session_ctrl = PlaybackSessionController::new(local_store.clone());

        // Suppose user watched 22 seconds earlier locally
        local_store
            .save_progress("local", "vid-2338", 22_000, duration_ms)
            .unwrap();

        // On Home screen, Jellyfin returns 51% completion (7,231,800,000 ticks)
        let home_item = TvItem {
            id: "vid-2338".into(),
            title: "Test 23:38 Video".into(),
            runtime_ticks: Some((duration_ms * 10_000) as i64),
            playback_position_ticks: 7_231_800_000, // 51% = ~723.18 seconds
            played_percentage: Some(51.0),
            ..Default::default()
        };

        // Ensure Home item progress_fraction calculates to 51%
        assert!((home_item.progress_fraction() - 0.51).abs() < 0.01);

        // When resuming, resolve_item_resume_position MUST resolve to 51% (~723 seconds),
        // NOT the old 22 seconds!
        let resolved_resume_ms = PlaybackSessionController::resolve_item_resume_position(
            &local_store,
            &session_ctrl,
            &home_item,
        );
        assert!(resolved_resume_ms.is_some());
        let resume_ms = resolved_resume_ms.unwrap();
        assert_eq!(resume_ms, 723_180);
        assert!(resume_ms > 700_000);
        assert_ne!(resume_ms, 22_000);

        // Verify should_prompt_resume accepts this position
        assert!(PlaybackSessionController::should_prompt_resume(
            resume_ms,
            duration_ms
        ));
    }

    #[test]
    fn test_download_capture_item_and_no_duplicates() {
        use std::sync::atomic::AtomicU64;
        let mgr = Arc::new(download::DownloadManager::new());
        let item1 = TvItem {
            id: "movie-abc".into(),
            title: "Movie ABC".into(),
            container: Some("mp4".into()),
            ..Default::default()
        };

        let temp_dir = std::env::temp_dir().join("ah_dl_test_nodup");
        let _ = std::fs::create_dir_all(&temp_dir);
        let target_file = temp_dir.join("Movie ABC.mp4");

        let status = Arc::new(Mutex::new(download::DownloadStatus::Downloading));
        let task = Arc::new(download::DownloadTask {
            item_id: item1.id.clone(),
            item_title: item1.title.clone(),
            target_path: target_file.clone(),
            temp_path: temp_dir.join("Movie ABC.mp4.part"),
            bytes_downloaded: Arc::new(AtomicU64::new(500)),
            total_bytes: Arc::new(AtomicU64::new(1000)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            pause_flag: Arc::new(AtomicBool::new(false)),
            supports_range: Arc::new(AtomicBool::new(false)),
            status: status.clone(),
            child_process: Arc::new(Mutex::new(None)),
        });

        mgr.set_task(task);

        // Active for item1
        assert!(mgr.is_active_or_queued_for_item("movie-abc"));
        // Duplicate check blocks item1
        assert!(mgr.is_downloading());

        // If player shuffles to item2, download task remains pinned to item1
        let item2_id = "movie-xyz";
        assert!(!mgr.is_active_or_queued_for_item(item2_id));
        let cur = mgr.current_task().unwrap();
        assert_eq!(cur.item_id, "movie-abc");
        assert_eq!(cur.item_title, "Movie ABC");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_performer_category_filtering() {
        let genre_lower: std::collections::HashSet<String> =
            vec!["exclusive".to_string(), "hd".to_string()]
                .into_iter()
                .collect();

        let is_category_word = |name: &str| -> bool {
            let lower = name.trim().to_lowercase();
            if lower.is_empty() || genre_lower.contains(&lower) {
                return true;
            }
            const KNOWN_CATEGORIES: &[&str] = &[
                "locker room",
                "body worship",
                "behind the scenes",
                "compilation",
                "casting",
                "threesome",
                "foursome",
                "solo",
                "interracial",
                "outdoor",
                "public",
                "pov",
                "point of view",
                "parody",
                "fetish",
                "foot fetish",
                "massage",
                "oil massage",
                "deepthroat",
                "blowjob",
                "creampie",
                "facial",
                "masturbation",
                "striptease",
                "bdsm",
                "bondage",
                "roleplay",
                "cosplay",
                "hardcore",
                "softcore",
                "amateur",
                "vintage",
                "retro",
                "uniform",
                "lingerie",
                "stockings",
                "anal",
                "oral",
                "glamour",
                "erotic",
            ];
            KNOWN_CATEGORIES.iter().any(|cat| lower == *cat)
        };

        let raw_names = vec![
            "Eva Green",
            "Locker room",
            "Body Worship",
            "Exclusive",
            "Mia Ray",
            "behind the scenes",
        ];

        let filtered: Vec<&str> = raw_names
            .into_iter()
            .filter(|n| !is_category_word(n))
            .collect();
        assert_eq!(filtered, vec!["Eva Green", "Mia Ray"]);
    }
}
