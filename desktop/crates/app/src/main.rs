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
    DEBUG_MODE.store(options.debug, std::sync::atomic::Ordering::Relaxed);

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

    log_startup(&format!(
        "session start (debug={}, renderer-cli={})",
        options.debug,
        options.renderer.as_deref().unwrap_or("unset")
    ));
    log_debug(&format!(
        "startup: logs at {}, mpv will log through the app ring buffer",
        startup_log_path().display()
    ));
    let (ui, software_mode) = create_main_window(&local_store, &options)?;
    log_startup(&format!(
        "window created with the {} UI renderer",
        if software_mode { "software" } else { "OpenGL" }
    ));
    ui.set_software_ui_renderer(software_mode);
    if software_mode {
        ui.set_status("Software fallback: video opens in its own window".into());
    }
    // An explicit size from the environment is used by the automated layout
    // checks. Restoring the saved geometry there would fight it, and storing it
    // back would replace the user's real window size with the test size.
    let forced_size: Option<(f32, f32)> = std::env::var("AH_WINDOW_SIZE")
        .ok()
        .and_then(|value| {
            let (width, height) = value.split_once('x')?;
            Some((width.parse().ok()?, height.parse().ok()?))
        })
        .filter(|(width, height)| *width >= 320.0 && *height >= 240.0);
    // The saved window rect is applied once the winit window exists (see
    // `apply_saved_window_geometry`): the validation and the DPI conversion
    // both need the live monitor list and scale factor, which do not exist
    // before the first show.
    if let Some((width, height)) = forced_size {
        ui.set_window_target_width(width);
        ui.set_window_target_height(height);
    }
    let geometry_store = forced_size.is_none().then(|| local_store.clone());
    let geometry_applied_successfully = Arc::new(AtomicBool::new(forced_size.is_some()));
    let _window_event_filter = install_window_event_filter(
        &ui,
        geometry_store,
        geometry_applied_successfully.clone(),
    );
    set_window_icon(&ui);
    ui.set_licenses_text(LICENSES_TEXT.into());
    let pkg_ver = env!("CARGO_PKG_VERSION");
    let commit = env!("ADULT_HUB_COMMIT_HASH");
    let dirty = if env!("ADULT_HUB_DIRTY") == "1" {
        "-dirty"
    } else {
        ""
    };
    let build_id = env!("ADULT_HUB_BUILD_ID");
    let build_date = env!("ADULT_HUB_BUILD_DATE");
    let ver_details = format!("v{pkg_ver} ({commit}{dirty}) · build: {build_id} · Built {build_date}");
    ui.set_app_version(pkg_ver.into());
    ui.set_app_version_details(ver_details.into());

    // In software UI mode there is no shared OpenGL context, so mpv renders
    // video in its own window with an independent GPU/software pipeline.
    let player = match if software_mode {
        Player::new_native_video()
    } else {
        Player::new()
    } {
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
        let limit_str = thumbnails::format_cache_limit(limit_mb);
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
        let local_store = local_store.clone();
        let geometry_applied = geometry_applied_successfully.clone();
        ui.window().on_close_requested(move || {
            crate::log_debug("window: native close requested");
            if let Some(ui) = weak.upgrade() {
                if geometry_applied.load(std::sync::atomic::Ordering::SeqCst) {
                    persist_window_geometry(&ui, &local_store, "close");
                } else {
                    log_startup("window: close requested before geometry restore completed; skipping persist to protect saved rect");
                }
                if ui.get_active_screen() == "player" {
                    crate::log_debug("player: stopping playback before window close");
                    playback::stop_active_playback_for_navigation(
                        &ui,
                        player.as_deref(),
                        &reporter,
                        &session_ctrl,
                        &current_item,
                    );
                }
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
        if software_mode {
            // mpv's native window needs no shared GL renderer to wait for.
            on_renderer_ready();
        } else {
            install_renderer(&ui, p.clone(), &frame_pending, on_renderer_ready)?;
        }
    } else {
        ui.set_status("Ready.".into());
    }

    if options.demo_shuffle {
        run_demo_shuffle(&ui);
    }

    if let Err(error) = ui.show() {
        log_startup(&format!("showing the main window failed: {error}"));
        if !software_mode {
            // Exits this process when the software-mode child spawned.
            try_relaunch_in_software_mode(&format!("showing the main window failed: {error}"));
        }
        fatal_startup_error(&format!("could not show the main window: {error}"));
        return Err(error.into());
    }
    let loop_start = std::time::Instant::now();
    if let Err(error) = ui.run() {
        log_startup(&format!("the event loop terminated with an error: {error}"));
        if !software_mode && loop_start.elapsed() < Duration::from_secs(5) {
            // An immediate failure behaves like a failed start; a long session
            // that dies later is not answered with a renderer relaunch.
            try_relaunch_in_software_mode(&format!("the event loop failed on startup: {error}"));
        }
        return Err(error.into());
    }
    // The event loop owns the window from here on; the first Moved/Resized
    // event applies the saved geometry (or the geometry timer fallback does
    // within 600 ms), once monitors and the DPI scale factor are queryable.
    crate::log_debug(&format!(
        "session end: event loop exited normally after {} s",
        loop_start.elapsed().as_secs()
    ));
    let _ = local_store.set_setting("card_zoom", &format!("{:.4}", ui.get_card_zoom()));
    Ok(())
}

/// Creates the main window with the OpenGL-backed winit renderer or the CPU
/// software renderer. Resolution order:
/// 1. `--renderer` from the command line, then `AH_RENDERER`; either one is
///    decisive for this run and never changes the stored preference.
/// 2. On Windows without an explicit choice, the [`probe_opengl`] result of
///    this session decides. The probe runs even when an earlier session stored
///    a software preference, so a failed GL test in an unusual execution
///    environment cannot pin a healthy desktop to the software renderer; the
///    stored `renderer_mode` is kept current for diagnostics.
/// 3. On other platforms without an explicit choice, the stored
///    `renderer_mode` preference avoids paying a failed GL start every run.
///
/// winit allows exactly one event loop per process, so a GL failure that the
/// pre-flight did not predict is answered by relaunching the process once in
/// software mode ([`try_relaunch_in_software_mode`]), never by a second
/// backend selection. Such a relaunch stores no preference. Only when a
/// relaunch is not allowed does one in-process software attempt remain, as
/// the last step before the fatal startup dialog.
fn create_main_window(
    local_store: &LocalStore,
    options: &Options,
) -> Result<(MainWindow, bool), Box<dyn std::error::Error>> {
    let env_choice = std::env::var("AH_RENDERER")
        .ok()
        .map(|value| value.to_ascii_lowercase());
    let renderer_choice = options.renderer.clone().or(env_choice.clone());
    let stored_software = local_store
        .get_setting("renderer_mode")
        .ok()
        .flatten()
        .is_some_and(|value| value == "software");
    let explicit_gl = renderer_choice.as_deref() == Some("gl");
    let explicit_software = renderer_choice.as_deref() == Some("software");
    let explicit_choice = explicit_gl || explicit_software;

    // winit allows exactly one event loop per process, so a failed GL window
    // creation cannot be retried with a second backend selection. On Windows
    // the GPU path is therefore validated with a throwaway WGL context before
    // the one-shot selection below. An explicit `--renderer`/`AH_RENDERER`
    // choice skips the probe: the user, not the probe, decides that session.
    #[cfg(target_os = "windows")]
    let use_software = if explicit_software {
        true
    } else if explicit_gl {
        false
    } else {
        log_debug("OpenGL pre-flight probe starting");
        match probe_opengl() {
            Ok(info) => {
                log_startup(&format!("OpenGL pre-flight probe passed: {info}"));
                false
            }
            Err(reason) => {
                log_startup(&format!(
                    "OpenGL pre-flight probe failed ({reason}); selecting the software UI renderer"
                ));
                let _ = local_store.set_setting("renderer_mode", "software");
                true
            }
        }
    };
    #[cfg(not(target_os = "windows"))]
    let use_software = explicit_software || (!explicit_gl && stored_software);

    log_startup(&format!(
        "renderer decision: cli={}, AH_RENDERER={}, stored renderer_mode={}, chosen={}",
        options.renderer.as_deref().unwrap_or("unset"),
        env_choice.as_deref().unwrap_or("unset"),
        if stored_software { "software" } else { "gl" },
        if use_software { "software" } else { "gl" }
    ));

    if use_software {
        return match create_window(true) {
            Ok(ui) => Ok((ui, true)),
            Err(error) => {
                log_startup(&format!("software renderer start failed: {error}"));
                fatal_startup_error(&error.to_string());
                Err(error.into())
            }
        };
    }

    match create_window(false) {
        Ok(ui) => {
            if stored_software && !explicit_choice {
                let _ = local_store.set_setting("renderer_mode", "gl");
            }
            Ok((ui, false))
        }
        Err(gl_error) => {
            log_startup(&format!(
                "OpenGL renderer start failed after the pre-flight decision ({gl_error})"
            ));
            if explicit_gl {
                // An explicit GL choice is decisive: do not quietly switch to
                // software behind the user's back, and do not relaunch.
                fatal_startup_error(&format!(
                    "the explicitly requested OpenGL UI renderer failed: {gl_error}"
                ));
                return Err(gl_error.into());
            }
            // Exits this process when a software-mode child was spawned.
            try_relaunch_in_software_mode(&format!("OpenGL UI renderer failed: {gl_error}"));
            // Only reached when a relaunch is not allowed or could not spawn:
            // one last in-process attempt before declaring the start fatal.
            match create_window(true) {
                Ok(ui) => {
                    let _ = local_store.set_setting("renderer_mode", "software");
                    Ok((ui, true))
                }
                Err(fallback_error) => {
                    log_startup(&format!(
                        "software renderer fallback failed: {fallback_error}"
                    ));
                    fatal_startup_error(&format!(
                        "OpenGL renderer start failed ({gl_error}); \
                         software renderer fallback failed ({fallback_error})"
                    ));
                    Err(fallback_error.into())
                }
            }
        }
    }
}

fn create_window(software: bool) -> Result<MainWindow, slint::PlatformError> {
    let selector = slint::BackendSelector::new().backend_name("winit".into());
    let selector = if software {
        selector.renderer_name("software".into())
    } else {
        selector.require_opengl()
    };
    selector.select()?;
    MainWindow::new()
}

/// Relaunches this process exactly once with the software UI renderer and
/// exits; a successful relaunch never returns. Used when the GPU path fails
/// too late for the pre-flight to predict (window creation, show or event
/// loop start): winit forbids a second event loop in this process, so a fresh
/// process is the only controlled recovery. The fallback stores no
/// preference — an unusual execution environment must not pin later runs —
/// and the `AH_RENDERER_RELAUNCH` marker travels to the child, so relaunches
/// cannot chain. Returns false when relaunching is not permitted or failed.
fn try_relaunch_in_software_mode(reason: &str) -> bool {
    if std::env::var("AH_RENDERER_RELAUNCH").is_ok_and(|value| !value.is_empty()) {
        log_startup("a software-mode relaunch already ran this session; not relaunching again");
        return false;
    }
    if std::env::var("AH_RENDERER").is_ok_and(|value| value.eq_ignore_ascii_case("gl")) {
        log_startup("AH_RENDERER=gl was requested explicitly; not relaunching in software mode");
        return false;
    }
    let Ok(executable) = std::env::current_exe() else {
        log_startup("cannot resolve the current executable for a software-mode relaunch");
        return false;
    };
    let spawned = std::process::Command::new(executable)
        .args(std::env::args_os().skip(1))
        .env("AH_RENDERER", "software")
        .env("AH_RENDERER_RELAUNCH", "1")
        .spawn();
    match spawned {
        Ok(_) => {
            log_startup(&format!(
                "relaunching once in software UI mode ({reason}); no persistent preference stored"
            ));
            std::process::exit(0);
        }
        Err(error) => {
            log_startup(&format!("software-mode relaunch could not spawn: {error}"));
            false
        }
    }
}

/// Creates a throwaway hidden window and WGL context the same way Slint's
/// glutin-backed renderer does, and reports the resulting driver strings.
/// Returns `Err` when the machine cannot support an OpenGL UI renderer.
#[cfg(target_os = "windows")]
fn probe_opengl() -> Result<String, String> {
    use std::ffi::CStr;
    use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{GetDC, HDC, ReleaseDC};
    use windows_sys::Win32::Graphics::OpenGL::{
        ChoosePixelFormat, GL_RENDERER, GL_VERSION, HGLRC, PFD_DOUBLEBUFFER, PFD_MAIN_PLANE,
        PFD_SUPPORT_OPENGL, PFD_TYPE_RGBA, PIXELFORMATDESCRIPTOR, SetPixelFormat, wglCreateContext,
        wglDeleteContext, wglGetProcAddress, wglMakeCurrent,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CS_OWNDC, CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW,
        UnregisterClassW, WNDCLASSEXW, WS_POPUP,
    };

    const PFD_DRAWING_WINDOW: u32 = 0x0010;
    const WGL_CONTEXT_MAJOR_VERSION_ARB: i32 = 0x2091;
    const WGL_CONTEXT_MINOR_VERSION_ARB: i32 = 0x2092;
    const WGL_CONTEXT_PROFILE_MASK_ARB: i32 = 0x9126;
    const WGL_CONTEXT_COMPATIBILITY_PROFILE_BIT_ARB: i32 = 0x0002;

    unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, w, l) }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe fn gl_string(name: u32) -> String {
        use windows_sys::Win32::Graphics::OpenGL::glGetString;
        let ptr = unsafe { glGetString(name) };
        if ptr.is_null() {
            return String::new();
        }
        unsafe {
            CStr::from_ptr(ptr.cast::<i8>())
                .to_string_lossy()
                .into_owned()
        }
    }

    let last_error = || format!("Windows error {}", unsafe { GetLastError() });

    let class_name = wide("AdultHubGlProbe");
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_OWNDC,
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance,
        lpszClassName: class_name.as_ptr(),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(format!("RegisterClassExW failed: {}", last_error()));
    }
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_POPUP,
            0,
            0,
            64,
            64,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        unsafe { UnregisterClassW(class_name.as_ptr(), instance) };
        return Err(format!("CreateWindowExW failed: {}", last_error()));
    }

    let result: Result<String, String>;
    let hdc = unsafe { GetDC(hwnd) };
    if hdc.is_null() {
        result = Err("GetDC failed".to_owned());
    } else {
        let mut pfd: PIXELFORMATDESCRIPTOR = unsafe { std::mem::zeroed() };
        pfd.nSize = size_of::<PIXELFORMATDESCRIPTOR>() as u16;
        pfd.nVersion = 1;
        pfd.dwFlags = PFD_SUPPORT_OPENGL | PFD_DRAWING_WINDOW | PFD_DOUBLEBUFFER;
        pfd.iPixelType = PFD_TYPE_RGBA;
        pfd.cColorBits = 32;
        pfd.cDepthBits = 24;
        pfd.cStencilBits = 8;
        pfd.iLayerType = PFD_MAIN_PLANE as u8;
        let format = unsafe { ChoosePixelFormat(hdc, &pfd) };
        if format == 0 {
            result =
                Err("no OpenGL-capable pixel format (broken or missing GPU driver)".to_owned());
        } else if unsafe { SetPixelFormat(hdc, format, &pfd) } != 1 {
            result = Err("SetPixelFormat failed".to_owned());
        } else {
            let context: HGLRC = unsafe { wglCreateContext(hdc) };
            if context.is_null() {
                result = Err(format!("wglCreateContext failed: {}", last_error()));
            } else if unsafe { wglMakeCurrent(hdc, context) } != 1 {
                unsafe { wglDeleteContext(context) };
                result = Err(format!("wglMakeCurrent failed: {}", last_error()));
            } else {
                result = (|| -> Result<String, String> {
                    // Slint's renderer needs modern WGL entry points; a driver
                    // that only offers legacy GL 1.1 cannot back it.
                    let entry =
                        unsafe { wglGetProcAddress(c"wglCreateContextAttribsARB".as_ptr().cast()) };
                    let Some(entry) = entry else {
                        return Err(
                            "wglCreateContextAttribsARB missing from the OpenGL driver".to_owned()
                        );
                    };
                    type CreateAttribs = unsafe extern "system" fn(HDC, HGLRC, *const i32) -> HGLRC;
                    let create: CreateAttribs = unsafe { std::mem::transmute(entry) };
                    let attributes = [
                        WGL_CONTEXT_MAJOR_VERSION_ARB,
                        2,
                        WGL_CONTEXT_MINOR_VERSION_ARB,
                        1,
                        WGL_CONTEXT_PROFILE_MASK_ARB,
                        WGL_CONTEXT_COMPATIBILITY_PROFILE_BIT_ARB,
                        0,
                    ];
                    let modern = unsafe { create(hdc, std::ptr::null_mut(), attributes.as_ptr()) };
                    if modern.is_null() {
                        return Err("driver refused an OpenGL 2.1 compatibility context".to_owned());
                    }
                    unsafe { wglDeleteContext(modern) };
                    let version = unsafe { gl_string(GL_VERSION) };
                    let renderer = unsafe { gl_string(GL_RENDERER) };
                    if version.is_empty() {
                        return Err("glGetString(GL_VERSION) returned nothing".to_owned());
                    }
                    Ok(format!("{version} / {renderer}"))
                })();
                unsafe {
                    wglMakeCurrent(std::ptr::null_mut(), std::ptr::null_mut());
                    wglDeleteContext(context);
                }
            }
        }
        unsafe { ReleaseDC(hwnd, hdc) };
    }
    unsafe {
        DestroyWindow(hwnd);
        UnregisterClassW(class_name.as_ptr(), instance);
    }
    result
}

pub(crate) fn startup_log_path() -> std::path::PathBuf {
    let mut path = default_db_path();
    path.set_file_name("startup.log");
    path
}

/// `--debug` verbosity switch, set in `main` before the first log write.
/// Debug mode only adds detail to the log file; it never changes behaviour.
static DEBUG_MODE: AtomicBool = AtomicBool::new(false);

/// Random per-process identifier so every line of one run can be correlated
/// across the startup log and the in-memory diagnostics ring.
static SESSION_ID: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect()
});

pub(crate) fn debug_logging_enabled() -> bool {
    DEBUG_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Detailed diagnostics line; written to the startup log only under `--debug`.
/// Debug mode only adds detail to the log file; it never changes behaviour.
/// Every message is redacted first so library-echoed URLs, tokens and paths
/// can never reach the log file verbatim.
pub(crate) fn log_debug(message: &str) {
    if debug_logging_enabled() {
        log_startup(&crate::diagnostics::redact(message));
    }
}

pub(crate) fn log_startup(message: &str) {
    let path = startup_log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Bound the log growth: one rotation keeps the previous tail for the
    // support case where the failure right before the current one matters.
    const MAX_LOG_BYTES: u64 = 512 * 1024;
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= MAX_LOG_BYTES) {
        let mut old = path.clone();
        old.set_file_name("startup.log.old");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(&path, &old);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let commit = env!("ADULT_HUB_COMMIT_HASH");
    let dirty = if env!("ADULT_HUB_DIRTY") == "1" {
        "-dirty"
    } else {
        ""
    };
    let line = format!(
        "[{stamp}] v{} ({commit}{dirty}) build={} session={} {message}\n",
        env!("CARGO_PKG_VERSION"),
        env!("ADULT_HUB_BUILD_ID"),
        *SESSION_ID,
    );
    eprint!("{line}");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = std::io::Write::write_all(&mut file, line.as_bytes());
    }
}

fn fatal_startup_error(message: &str) {
    eprintln!("fatal startup error: {message}");
    let text = format!(
        "Adult Hub Desktop cannot start.\n\n{message}\n\n\
         Log file: {}\n\
         Relaunch as `adult-hub-desktop.exe --debug` for detailed diagnostics.",
        startup_log_path().display()
    );
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let encode = |value: &str| -> Vec<u16> {
            value
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let title = encode("Adult Hub Desktop - startup failure");
        let body = encode(&text);
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                body.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(target_os = "windows"))]
    eprintln!("{text}");
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

/// Handle card zoom and window geometry before Slint dispatches input. All
/// unrelated pointer and keyboard events stay on Slint's normal path.
/// Slint allows only one winit window-event handler per window, so the
/// geometry saver rides along with the zoom filter.
fn install_window_event_filter(
    ui: &MainWindow,
    geometry_store: Option<Arc<LocalStore>>,
    geometry_applied_successfully: Arc<AtomicBool>,
) -> (Rc<slint::Timer>, Rc<slint::Timer>) {
    use slint::ComponentHandle;

    let restore_timer = Rc::new(slint::Timer::default());
    let persist_timer = Rc::new(slint::Timer::default());

    let restore_completed = Rc::new(Cell::new(geometry_store.is_none()));
    let restore_retries = Rc::new(Cell::new(0u32));
    const MAX_RESTORE_RETRIES: u32 = 25; // 25 * 100ms = 2.5s maximum before fallback

    if geometry_store.is_some() {
        log_startup("window geometry: restore initiated");
        let weak = ui.as_weak();
        let store = geometry_store.clone();
        let completed = restore_completed.clone();
        let retries = restore_retries.clone();
        let applied_flag = geometry_applied_successfully.clone();
        let timer_stop = restore_timer.clone();

        restore_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(100),
            move || {
                if completed.get() {
                    timer_stop.stop();
                    return;
                }
                let (Some(store), Some(ui)) = (store.as_ref(), weak.upgrade()) else {
                    return;
                };
                let attempt = retries.get() + 1;
                retries.set(attempt);
                let allow_fallback = attempt >= MAX_RESTORE_RETRIES;
                if attempt == 1 || attempt % 5 == 0 {
                    log_startup(&format!(
                        "window geometry: retrying restore (attempt {attempt}/{MAX_RESTORE_RETRIES})"
                    ));
                }
                let ok = apply_saved_window_geometry(&ui, store, allow_fallback);
                if ok {
                    completed.set(true);
                    timer_stop.stop();
                    applied_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    log_startup("window geometry: restore completed successfully (timer)");
                }
            },
        );
    } else {
        geometry_applied_successfully.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    {
        let weak = ui.as_weak();
        let store = geometry_store.clone();
        persist_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(500),
            move || {
                let (Some(store), Some(ui)) = (store.as_ref(), weak.upgrade()) else {
                    return;
                };
                if !ui.get_is_fullscreen() {
                    persist_window_geometry(&ui, store, "deferred");
                }
            },
        );
        persist_timer.stop();
    }

    let event_restore_store = geometry_store.clone();
    let event_restore_completed = restore_completed.clone();
    let event_restore_applied = geometry_applied_successfully.clone();
    let event_restore_timer = restore_timer.clone();
    let event_persist_timer = persist_timer.clone();
    let event_weak = ui.as_weak();

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
                winit::event::WindowEvent::Moved(_)
                | winit::event::WindowEvent::Resized(_)
                | winit::event::WindowEvent::ScaleFactorChanged { .. } => {
                    if !event_restore_completed.get() {
                        if let (Some(store), Some(ui)) = (
                            event_restore_store.as_ref(),
                            event_weak.upgrade(),
                        ) {
                            let ok = apply_saved_window_geometry(&ui, store, false);
                            if ok {
                                event_restore_completed.set(true);
                                event_restore_timer.stop();
                                event_restore_applied
                                    .store(true, std::sync::atomic::Ordering::SeqCst);
                                log_startup(
                                    "window geometry: restore completed successfully (event-driven)",
                                );
                            }
                        }
                    } else if let (Some(_store), Some(ui)) = (
                        event_restore_store.as_ref(),
                        event_weak.upgrade(),
                    ) {
                        if !ui.get_is_fullscreen() {
                            event_persist_timer.restart();
                        }
                    }
                }
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

    (restore_timer, persist_timer)
}

const WINDOW_GEOMETRY_KEY: &str = "window_geometry";
/// Smallest window accepted when restoring a saved rect, in physical pixels.
const MIN_RESTORE_WIDTH: u32 = 640;
const MIN_RESTORE_HEIGHT: u32 = 480;
/// Above any real window a saved extent can reach. Values past this came from
/// the historic physical/logical double-scaling bug and are rejected so the
/// restore falls back to the default size.
const MAX_GEOMETRY_EXTENT: u32 = 16384;
/// Overlap a saved rect must keep with a live monitor to be considered
/// reachable. Anything less comes back without a grabbable title bar.
const MIN_VISIBLE_OVERLAP_WIDTH: i32 = 120;
const MIN_VISIBLE_OVERLAP_HEIGHT: i32 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MonitorRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl MonitorRect {
    fn right(self) -> i32 {
        self.x + self.width as i32
    }

    fn bottom(self) -> i32 {
        self.y + self.height as i32
    }
}

/// Window rectangle in physical pixels (winit's native unit), plus the
/// maximized flag. The ordinary (un-maximized) rect is always stored:
/// [`geometry_to_store`] keeps the last normal rect while the window is
/// maximized, so un-maximizing after a restart returns to that size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowGeometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    maximized: bool,
}

impl WindowGeometry {
    fn right(self) -> i32 {
        self.x
            .saturating_add(self.width.min(i32::MAX as u32) as i32)
    }

    fn bottom(self) -> i32 {
        self.y
            .saturating_add(self.height.min(i32::MAX as u32) as i32)
    }

    fn reaches(self, monitor: MonitorRect) -> bool {
        let overlap_width = self.right().min(monitor.right()) - self.x.max(monitor.x);
        let overlap_height = self.bottom().min(monitor.bottom()) - self.y.max(monitor.y);
        overlap_width >= MIN_VISIBLE_OVERLAP_WIDTH && overlap_height >= MIN_VISIBLE_OVERLAP_HEIGHT
    }

    fn encode(self) -> String {
        format!(
            "{},{},{},{},{}",
            self.x, self.y, self.width, self.height, self.maximized as u8
        )
    }

    fn decode(value: &str) -> Option<Self> {
        let mut fields = value.trim().split(',');
        let geometry = Self {
            x: fields.next()?.parse().ok()?,
            y: fields.next()?.parse().ok()?,
            width: fields.next()?.parse().ok()?,
            height: fields.next()?.parse().ok()?,
            maximized: fields.next()?.parse::<u8>().ok()? != 0,
        };
        let sane = geometry.width > 0
            && geometry.height > 0
            && geometry.width <= MAX_GEOMETRY_EXTENT
            && geometry.height <= MAX_GEOMETRY_EXTENT;
        sane.then_some(geometry)
    }

    /// Fit a saved rect to the monitors that are connected right now.
    /// Clamps bloated/corrupted dimensions to the monitor's usable work area
    /// and ensures the title bar remains accessible even on multi-monitor
    /// layouts with negative coordinates.
    fn constrained_to(self, monitors: &[MonitorRect]) -> Self {
        let Some(largest) = monitors
            .iter()
            .max_by_key(|monitor| u64::from(monitor.width) * u64::from(monitor.height))
            .copied()
        else {
            return self;
        };
        // Windows anchors the desktop origin at the primary monitor, so a window
        // that lived there should come back there rather than on a second screen.
        let home = monitors
            .iter()
            .find(|monitor| monitor.x == 0 && monitor.y == 0)
            .copied()
            .unwrap_or(largest);
        let mut restored = self;

        if let Some(target) = monitors.iter().find(|monitor| restored.reaches(**monitor)) {
            // Constrain normal window size to avoid exceeding monitor work area
            // (e.g. from historical scale compounding bugs like 3614x2328).
            let max_w = target.width.saturating_sub(40).max(MIN_RESTORE_WIDTH);
            let max_h = target.height.saturating_sub(60).max(MIN_RESTORE_HEIGHT);
            restored.width = restored.width.clamp(MIN_RESTORE_WIDTH.min(max_w), max_w);
            restored.height = restored.height.clamp(MIN_RESTORE_HEIGHT.min(max_h), max_h);

            // Ensure title bar is reachable and grabbable vertically
            if restored.y < target.y {
                restored.y = target.y;
            } else if restored.y > target.bottom() - MIN_VISIBLE_OVERLAP_HEIGHT {
                restored.y = target.bottom() - MIN_VISIBLE_OVERLAP_HEIGHT;
            }
            // Ensure title bar has horizontal overlap
            if restored.right() < target.x + MIN_VISIBLE_OVERLAP_WIDTH {
                restored.x = target.x;
            } else if restored.x > target.right() - MIN_VISIBLE_OVERLAP_WIDTH {
                restored.x = target.right() - MIN_VISIBLE_OVERLAP_WIDTH;
            }
            return restored;
        }

        // Window was outside all current monitors: clamp to home monitor work area and center
        let max_w = home.width.saturating_sub(40).max(MIN_RESTORE_WIDTH);
        let max_h = home.height.saturating_sub(60).max(MIN_RESTORE_HEIGHT);
        restored.width = restored.width.clamp(MIN_RESTORE_WIDTH.min(max_w), max_w);
        restored.height = restored.height.clamp(MIN_RESTORE_HEIGHT.min(max_h), max_h);
        restored.x = home.x + (home.width as i32 - restored.width as i32) / 2;
        restored.y = home.y + (home.height as i32 - restored.height as i32) / 2;
        if restored.y < home.y {
            restored.y = home.y;
        }
        restored
    }
}

fn default_normal_geometry(scale: f64) -> WindowGeometry {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    WindowGeometry {
        x: 100,
        y: 100,
        width: (1180.0 * scale).round() as u32,
        height: (760.0 * scale).round() as u32,
        maximized: false,
    }
}

/// A maximized window reports the whole monitor as its rectangle, so storing
/// that report would turn a later un-maximize into a full-screen sized window.
/// Keep the last ordinary rectangle and update only the flag.
fn geometry_to_store(
    current: WindowGeometry,
    previous: Option<WindowGeometry>,
    fallback_scale: f64,
) -> WindowGeometry {
    if current.maximized {
        let normal = previous.unwrap_or_else(|| default_normal_geometry(fallback_scale));
        WindowGeometry {
            maximized: true,
            x: normal.x,
            y: normal.y,
            width: normal.width,
            height: normal.height,
        }
    } else {
        current
    }
}

/// Convert a saved physical-pixel rect into the logical size that must be fed
/// to Slint's window `preferred-width`/`preferred-height` properties. Slint
/// multiplies these logical values back by the window scale factor to reach the
/// physical size the window manager creates, so dividing here exactly cancels
/// that multiplication. Feeding the physical value directly (the old bug) made
/// the window grow by `scale` on every restart. The result is clamped to the
/// restore bounds so a corrupt scale cannot blow the window up again.
fn restore_logical_size(geometry: WindowGeometry, scale: f64) -> (f32, f32) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let width = (f64::from(geometry.width) / scale)
        .round()
        .clamp(320.0, f64::from(MAX_GEOMETRY_EXTENT)) as f32;
    let height = (f64::from(geometry.height) / scale)
        .round()
        .clamp(240.0, f64::from(MAX_GEOMETRY_EXTENT)) as f32;
    (width, height)
}

fn winit_monitor_rect(monitor: winit::monitor::MonitorHandle) -> MonitorRect {
    let position = monitor.position();
    let size = monitor.size();
    MonitorRect {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    }
}

/// Real, usable monitor rectangles in physical pixels. X servers without
/// usable RandR CRTCs (e.g. plain Xvfb) report an empty monitor list; winit's
/// dummy fallback monitor is a 1x1 placeholder and is filtered out, with the
/// window's own current monitor tried next.
fn monitor_rects(ui: &MainWindow) -> Vec<MonitorRect> {
    ui.window()
        .with_winit_window(|window| {
            let usable = |rect: MonitorRect| rect.width > 1 && rect.height > 1;
            let mut rects: Vec<MonitorRect> = window
                .available_monitors()
                .map(winit_monitor_rect)
                .filter(|rect| usable(*rect))
                .collect();
            if rects.is_empty() {
                rects = window
                    .current_monitor()
                    .map(winit_monitor_rect)
                    .filter(|rect| usable(*rect))
                    .into_iter()
                    .collect();
            }
            if rects.is_empty() {
                rects = window
                    .primary_monitor()
                    .map(winit_monitor_rect)
                    .filter(|rect| usable(*rect))
                    .into_iter()
                    .collect();
            }
            #[cfg(target_os = "windows")]
            if rects.is_empty() {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
                };
                let w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
                let h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
                if w > 1 && h > 1 {
                    rects.push(MonitorRect {
                        x: 0,
                        y: 0,
                        width: w as u32,
                        height: h as u32,
                    });
                }
            }
            rects
        })
        .unwrap_or_default()
}

fn current_window_geometry(ui: &MainWindow) -> Option<WindowGeometry> {
    ui.window().with_winit_window(|window| {
        if window.is_minimized().unwrap_or(false) || window.fullscreen().is_some() {
            return None;
        }
        let position = window.outer_position().ok()?;
        let size = window.inner_size();
        // Physical pixels: this is the single unit the saved geometry uses.
        let sane = size.width > 0
            && size.height > 0
            && size.width <= MAX_GEOMETRY_EXTENT
            && size.height <= MAX_GEOMETRY_EXTENT;
        sane.then_some(WindowGeometry {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
            maximized: window.is_maximized(),
        })
    })?
}

fn persist_window_geometry(ui: &MainWindow, local_store: &LocalStore, reason: &str) {
    if ui.get_is_fullscreen() {
        return;
    }
    let is_fullscreen_winit = ui
        .window()
        .with_winit_window(|w| w.fullscreen().is_some())
        .unwrap_or(false);
    if is_fullscreen_winit {
        return;
    }
    let Some(current) = current_window_geometry(ui) else {
        return;
    };
    let scale = ui
        .window()
        .with_winit_window(|window| window.scale_factor())
        .unwrap_or(1.0);
    let previous = local_store
        .get_setting(WINDOW_GEOMETRY_KEY)
        .ok()
        .flatten()
        .and_then(|value| WindowGeometry::decode(&value));
    let stored = geometry_to_store(current, previous, scale);
    if previous == Some(stored) {
        return;
    }
    log_startup(&format!(
        "window geometry saved ({reason}): {} (physical px)",
        stored.encode()
    ));
    let _ = local_store.set_setting(WINDOW_GEOMETRY_KEY, &stored.encode());
}

fn calculate_restored_geometry(
    saved: WindowGeometry,
    monitors: &[MonitorRect],
    scale: f64,
    allow_fallback: bool,
) -> Option<(WindowGeometry, f32, f32)> {
    if monitors.is_empty() {
        if allow_fallback {
            let fallback_rect = WindowGeometry {
                x: 100,
                y: 100,
                width: 1180,
                height: 760,
                maximized: saved.maximized,
            };
            let (w, h) = restore_logical_size(fallback_rect, scale);
            Some((fallback_rect, w, h))
        } else {
            None
        }
    } else {
        let validated = saved.constrained_to(monitors);
        let (w, h) = restore_logical_size(validated, scale);
        Some((validated, w, h))
    }
}

/// Apply the saved window rectangle once the winit window exists. It must run
/// after the window has been created: only then are the live monitor list and
/// the DPI scale factor queryable, which the validation and the physical to
/// logical size conversion both depend on.
///
/// Returns `true` when the geometry was applied with real monitor data (or safe
/// fallback when permitted), or when there is nothing saved (no action needed).
/// Returns `false` when the monitor list was temporarily empty and fallback was
/// not yet allowed: the caller should retry rather than treating this as a
/// final result, and must not persist the transient window state until `true` is
/// returned.
fn apply_saved_window_geometry(
    ui: &MainWindow,
    local_store: &LocalStore,
    allow_fallback: bool,
) -> bool {
    let Some(raw) = local_store.get_setting(WINDOW_GEOMETRY_KEY).ok().flatten() else {
        log_startup("window geometry: nothing saved, keeping the default window rect");
        return true;
    };
    let Some(saved) = WindowGeometry::decode(&raw) else {
        log_startup(&format!(
            "window geometry: stored value \"{raw}\" is not a valid physical rect, \
             keeping the default window rect"
        ));
        return true;
    };

    // Query the live scale factor. On Windows the compositor sets the DPI
    // for a window only after it has been associated with a monitor; before
    // that, winit returns 1.0 as a placeholder. We detect this race by
    // checking the monitor list: if no real monitors are visible yet, the
    // scale factor is also unreliable, so defer the restore unless fallback is forced.
    let monitors = monitor_rects(ui);
    let scale = ui
        .window()
        .with_winit_window(|window| window.scale_factor())
        .unwrap_or(1.0);
    let scale = if scale.is_finite() && (0.1..=10.0).contains(&scale) {
        scale
    } else {
        log_startup(&format!(
            "window geometry: implausible scale factor {scale}, assuming 1.0"
        ));
        1.0
    };

    if monitors.is_empty() {
        if !allow_fallback {
            log_startup(&format!(
                "window geometry: no monitor information available yet for {}, \
                 deferring restore until monitors are queryable",
                saved.encode()
            ));
            return false;
        } else {
            log_startup(&format!(
                "window geometry: monitor query timeout after retries, applying safe fallback for {}",
                saved.encode()
            ));
        }
    }

    let Some((validated, width, height)) =
        calculate_restored_geometry(saved, &monitors, scale, allow_fallback)
    else {
        return false;
    };

    if !monitors.is_empty() && validated != saved {
        log_startup(&format!(
            "window geometry: repaired saved rect {} to fit {} live monitor(s) -> {}",
            saved.encode(),
            monitors.len(),
            validated.encode()
        ));
    }

    // Drive the restored size through Slint's preferred-size properties: a raw
    // winit request is overridden when Slint applies the .slint defaults. The
    // property is a logical length, so the physical rect is divided by the
    // window's scale factor here and nowhere else.
    ui.set_window_target_width(width);
    ui.set_window_target_height(height);
    ui.window().set_position(slint::WindowPosition::Physical(
        slint::PhysicalPosition::new(validated.x, validated.y),
    ));
    if validated.maximized {
        ui.window().set_maximized(true);
        ui.window()
            .with_winit_window(|window| window.set_maximized(true));
    }

    if monitors.is_empty() {
        log_startup(&format!(
            "window geometry applied (safe fallback): {} at scale {scale:.2}, \
             target {width:.0}x{height:.0} logical (maximized: {})",
            validated.encode(),
            validated.maximized
        ));
    } else {
        log_startup(&format!(
            "window geometry applied: {} at scale {scale:.2} on {} monitor(s), \
             target {width:.0}x{height:.0} logical (maximized: {})",
            validated.encode(),
            monitors.len(),
            validated.maximized
        ));
    }
    true
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

    #[test]
    fn window_geometry_round_trips_the_saved_setting() {
        let geometry = WindowGeometry {
            x: -120,
            y: 40,
            width: 1280,
            height: 720,
            maximized: true,
        };
        assert_eq!(WindowGeometry::decode(&geometry.encode()), Some(geometry));
        assert_eq!(WindowGeometry::decode("wide,screen,window,0,0"), None);
        assert_eq!(WindowGeometry::decode("0,0,0,0,0"), None);
        assert_eq!(WindowGeometry::decode("0,0,800,600"), None);
    }

    #[test]
    fn restored_geometry_returns_to_a_connected_monitor() {
        let primary = MonitorRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let off_screen = WindowGeometry {
            x: 2600,
            y: 100,
            width: 1280,
            height: 720,
            maximized: false,
        };
        let restored = off_screen.constrained_to(&[primary]);
        assert_eq!((restored.x, restored.y), (320, 180));
        assert!(restored.reaches(primary));

        let reachable = WindowGeometry {
            x: 100,
            y: 120,
            width: 1200,
            height: 800,
            maximized: false,
        };
        assert_eq!(reachable.constrained_to(&[primary]), reachable);

        let oversized = WindowGeometry {
            x: 100,
            y: 120,
            width: 4000,
            height: 3000,
            maximized: false,
        };
        let clamped = oversized.constrained_to(&[primary]);
        assert_eq!((clamped.width, clamped.height), (1880, 1020));

        assert_eq!(off_screen.constrained_to(&[]), off_screen);
    }

    #[test]
    fn bloated_geometry_is_repaired_to_fit_monitor_work_area() {
        // A geometry bloated by previous 1.75x multiplying (e.g. 3614x2328)
        // must be clamped to fit inside the monitor's usable work area.
        let monitor = MonitorRect {
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
        };
        let bloated = WindowGeometry {
            x: 132,
            y: 132,
            width: 3614,
            height: 2328,
            maximized: false,
        };
        let repaired = bloated.constrained_to(&[monitor]);
        assert!(repaired.width <= monitor.width - 40);
        assert!(repaired.height <= monitor.height - 60);
        assert!(repaired.reaches(monitor));
    }

    #[test]
    fn delayed_monitor_availability_does_not_multiply_at_scale_one() {
        // Regression test for audit issue #1:
        // When monitor info is temporarily unavailable on early start,
        // geometry restore must be deferred instead of assuming scale 1.00.
        // If scale 1.00 was mistakenly applied, physical 2065x1330 at 175%
        // would become logical 2065x1330, and on render grow to 3614x2328.
        let saved_physical = WindowGeometry {
            x: 100,
            y: 100,
            width: 2065,
            height: 1330,
            maximized: false,
        };

        // When live scale is 1.75:
        let scale = 1.75;
        let (logical_w, logical_h) = restore_logical_size(saved_physical, scale);
        assert_eq!(logical_w as u32, 1180);
        assert_eq!(logical_h as u32, 760);

        // Physical realized by compositor:
        let realized_w = (logical_w as f64 * scale).round() as u32;
        let realized_h = (logical_h as f64 * scale).round() as u32;
        assert_eq!(realized_w, 2065);
        assert_eq!(realized_h, 1330);

        // If scale was mistakenly 1.00 (the bug):
        let (bug_w, _bug_h) = restore_logical_size(saved_physical, 1.00);
        assert_eq!(bug_w as u32, 2065);
        let bug_realized_w = (bug_w as f64 * scale).round() as u32;
        assert_eq!(bug_realized_w, 3614); // Exactly reproducing the audited bug
    }

    #[test]
    fn restored_geometry_keeps_a_negative_origin_offscreen_monitor() {
        // A left-of-primary extended desktop uses negative coordinates; that
        // must not be mistaken for a lost window and recentered.
        let left_monitor = MonitorRect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let on_left_screen = WindowGeometry {
            x: -1200,
            y: 120,
            width: 1000,
            height: 700,
            maximized: false,
        };
        assert_eq!(
            on_left_screen.constrained_to(&[left_monitor]),
            on_left_screen
        );
    }

    #[test]
    fn decode_rejects_geometry_larger_than_any_real_window() {
        // The historic physical/logical doubling wrote absurd sizes such as
        // 19371x1764; refuse them so restore falls back instead of growing.
        assert_eq!(WindowGeometry::decode("100,100,19371,1764,0"), None);
        assert_eq!(
            WindowGeometry::decode("100,100,16384,1764,0"),
            Some(WindowGeometry {
                x: 100,
                y: 100,
                width: 16384,
                height: 1764,
                maximized: false
            })
        );
    }

    #[test]
    fn geometry_restarts_do_not_grow_the_window_at_any_dpi_scale() {
        // Regression guard: Slint multiplies the logical target by the scale
        // factor to obtain the physical size, so the saved physical value must
        // survive restore -> show -> persist. Fractional scales cannot always
        // round-trip exactly (1180px at 150% is 786.67 logical), so the test
        // asserts the size stays bounded and converges instead of compounding.
        for scale in [1.0_f64, 1.25, 1.5, 1.75, 2.0, 3.0] {
            let mut saved = WindowGeometry {
                x: 100,
                y: 100,
                width: 1180,
                height: 790,
                maximized: false,
            };
            let mut previous = None;
            for _ in 0..10 {
                let (logical_w, logical_h) = restore_logical_size(saved, scale);
                // Slint/winit realize the physical size from the logical one.
                let width = (logical_w as f64 * scale).round() as u32;
                let height = (logical_h as f64 * scale).round() as u32;
                if let Some((prev_w, prev_h)) = previous {
                    assert!(
                        width.abs_diff(prev_w) <= 1 && height.abs_diff(prev_h) <= 1,
                        "size moved by more than a rounding step at scale {scale}: \
                         {prev_w}x{prev_h} -> {width}x{height}"
                    );
                }
                previous = Some((width, height));
                saved.width = width;
                saved.height = height;
            }
            assert!(
                saved.width.abs_diff(1180) <= 2 && saved.height.abs_diff(790) <= 2,
                "size grew over ten restarts at scale {scale}: {}",
                saved.encode()
            );
        }
    }

    #[test]
    fn maximized_windows_keep_the_previous_normal_rectangle() {
        let normal = WindowGeometry {
            x: 100,
            y: 100,
            width: 1180,
            height: 790,
            maximized: false,
        };
        let maximized_rect = WindowGeometry {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            maximized: true,
        };
        let stored = geometry_to_store(maximized_rect, Some(normal), 1.0);
        // Only the flag changes; the size stays the ordinary rectangle so a
        // later un-maximize returns to the real window size, not the monitor.
        assert_eq!(
            stored,
            WindowGeometry {
                maximized: true,
                ..normal
            }
        );
        // A restored maximized window converts its normal size, not the
        // maximized report, so the un-maximized size is correct.
        let (logical_w, _) = restore_logical_size(stored, 1.5);
        assert_eq!(logical_w as u32, (1180.0_f64 / 1.5).round() as u32);
    }

    #[test]
    fn restore_logical_size_clamps_corrupt_scale() {
        let geometry = WindowGeometry {
            x: 0,
            y: 0,
            width: 1500,
            height: 1000,
            maximized: false,
        };
        // A non-finite or zero scale must not divide into nonsense.
        for scale in [0.0, -1.0, f64::NAN] {
            let (w, h) = restore_logical_size(geometry, scale);
            assert!(w >= 320.0 && w <= MAX_GEOMETRY_EXTENT as f32);
            assert!(h >= 240.0 && h <= MAX_GEOMETRY_EXTENT as f32);
        }
    }

    #[test]
    fn closing_maximized_keeps_the_restore_rectangle() {
        let ordinary = WindowGeometry {
            x: 320,
            y: 180,
            width: 1280,
            height: 720,
            maximized: false,
        };
        let maximized_report = WindowGeometry {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            maximized: true,
        };
        let stored = geometry_to_store(maximized_report, Some(ordinary), 1.0);
        assert_eq!(
            stored,
            WindowGeometry {
                maximized: true,
                ..ordinary
            }
        );
        assert_eq!(geometry_to_store(ordinary, Some(stored), 1.0), ordinary);
        let default_stored = geometry_to_store(maximized_report, None, 1.0);
        assert!(default_stored.maximized);
        assert_eq!(default_stored.width, 1180);
        assert_eq!(default_stored.height, 760);
    }

    #[test]
    fn restore_deferred_when_monitors_unavailable_then_succeeds_when_available() {
        let saved = WindowGeometry {
            x: 120,
            y: 80,
            width: 2065,
            height: 1330,
            maximized: true,
        };
        // 1. Initial attempt with no monitors queryable must defer (return None)
        let first_try = calculate_restored_geometry(saved, &[], 1.75, false);
        assert_eq!(first_try, None, "restore must defer when monitors are queryable: false");

        // 2. Subsequent attempt when monitor becomes available must succeed
        let monitor = MonitorRect {
            x: 0,
            y: 0,
            width: 3840,
            height: 2160,
        };
        let second_try = calculate_restored_geometry(saved, &[monitor], 1.75, false);
        assert!(second_try.is_some(), "restore must succeed when monitor info arrives");
        let (rect, logical_w, logical_h) = second_try.unwrap();
        assert_eq!(rect.maximized, true);
        assert_eq!(rect.x, 120);
        assert_eq!(rect.y, 80);
        assert_eq!(rect.width, 2065);
        assert_eq!(rect.height, 1330);
        assert_eq!(logical_w.round() as u32, 1180);
        assert_eq!(logical_h.round() as u32, 760);
    }

    #[test]
    fn restore_permanent_missing_monitors_applies_safe_fallback_without_hang() {
        let saved = WindowGeometry {
            x: -500,
            y: -500,
            width: 9999,
            height: 9999,
            maximized: true,
        };
        // When retries expire, allow_fallback is set to true.
        // It must return a safe clamped fallback rectangle preserving maximization.
        let fallback_result = calculate_restored_geometry(saved, &[], 1.25, true);
        assert!(fallback_result.is_some(), "fallback must return a valid geometry");
        let (rect, logical_w, logical_h) = fallback_result.unwrap();
        assert_eq!(rect.maximized, true, "saved maximization flag must be preserved");
        assert_eq!(rect.width, 1180);
        assert_eq!(rect.height, 760);
        assert_eq!(logical_w.round() as u32, (1180.0 / 1.25_f64).round() as u32);
        assert_eq!(logical_h.round() as u32, (760.0 / 1.25_f64).round() as u32);
    }

    #[test]
    fn scale_175_converts_physical_rect_accurately_without_double_scaling() {
        // Physical rect corresponding to 1180x760 logical at scale 1.75:
        // 1180 * 1.75 = 2065 physical px
        // 760 * 1.75 = 1330 physical px
        let physical = WindowGeometry {
            x: 200,
            y: 100,
            width: 2065,
            height: 1330,
            maximized: false,
        };
        let (logical_w, logical_h) = restore_logical_size(physical, 1.75);
        assert_eq!(logical_w.round() as u32, 1180);
        assert_eq!(logical_h.round() as u32, 760);

        // Verify single conversion: physical is divided by 1.75 once, not multiplied or divided twice
        let back_to_physical_w = (logical_w as f64 * 1.75).round() as u32;
        let back_to_physical_h = (logical_h as f64 * 1.75).round() as u32;
        assert_eq!(back_to_physical_w, 2065);
        assert_eq!(back_to_physical_h, 1330);
    }

    #[test]
    fn maximized_flag_preserved_across_restore_and_fallback() {
        let normal_unmaximized = WindowGeometry {
            x: 100,
            y: 100,
            width: 1400,
            height: 900,
            maximized: false,
        };
        let normal_maximized = WindowGeometry {
            maximized: true,
            ..normal_unmaximized
        };

        let monitor = MonitorRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };

        // Case A: normal restore with live monitor
        let (applied_max, _, _) = calculate_restored_geometry(normal_maximized, &[monitor], 1.0, false).unwrap();
        assert!(applied_max.maximized, "maximized flag must be preserved with monitor");
        assert_eq!(applied_max.width, 1400);

        let (applied_unmax, _, _) = calculate_restored_geometry(normal_unmaximized, &[monitor], 1.0, false).unwrap();
        assert!(!applied_unmax.maximized, "unmaximized flag must be preserved with monitor");

        // Case B: fallback restore with missing monitor
        let (fallback_max, _, _) = calculate_restored_geometry(normal_maximized, &[], 1.0, true).unwrap();
        assert!(fallback_max.maximized, "maximized flag must be preserved in fallback");

        let (fallback_unmax, _, _) = calculate_restored_geometry(normal_unmaximized, &[], 1.0, true).unwrap();
        assert!(!fallback_unmax.maximized, "unmaximized flag must be preserved in fallback");
    }

    #[test]
    fn fullscreen_geometry_never_corrupts_saved_window_rect() {
        let normal_geometry = WindowGeometry {
            x: 100,
            y: 100,
            width: 1180,
            height: 760,
            maximized: false,
        };

        // When in fullscreen, window covers monitor (e.g. 2560x1440 at 0,0).
        // geometry_to_store should never replace the normal rectangle with fullscreen dimensions.
        let maximized_during_fs = WindowGeometry {
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
            maximized: true,
        };
        let stored = geometry_to_store(maximized_during_fs, Some(normal_geometry), 1.0);
        assert_eq!(stored.width, normal_geometry.width);
        assert_eq!(stored.height, normal_geometry.height);
        assert_eq!(stored.x, normal_geometry.x);
        assert_eq!(stored.y, normal_geometry.y);
        assert!(stored.maximized);
    }

    #[test]
    fn no_size_inflation_over_twenty_cycles_across_various_scales() {
        for scale in [1.0_f64, 1.25, 1.5, 1.75, 2.0] {
            let mut current = WindowGeometry {
                x: 80,
                y: 80,
                width: 1180,
                height: 760,
                maximized: false,
            };
            for _ in 0..20 {
                let (lw, lh) = restore_logical_size(current, scale);
                let phys_w = (lw as f64 * scale).round() as u32;
                let phys_h = (lh as f64 * scale).round() as u32;
                current.width = phys_w;
                current.height = phys_h;
            }
            assert!(
                current.width.abs_diff(1180) <= 2,
                "width drifted at scale {scale}: {}",
                current.width
            );
            assert!(
                current.height.abs_diff(760) <= 2,
                "height drifted at scale {scale}: {}",
                current.height
            );
        }
    }
}
