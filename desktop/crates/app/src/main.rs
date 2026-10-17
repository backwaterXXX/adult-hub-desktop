#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(clippy::too_many_arguments)]

mod renderer;
mod reporter;
mod session;

use adult_hub_client::{
    discover_servers, GetItemsQuery, JellyfinClient, ResolverClient, ServiceUrlPolicy,
    ServiceUrlValidation, TvItem,
};
use adult_hub_player::{BufferProfile, PlaybackStats, Player, parse_header};
use adult_hub_storage::{CredentialStore, LocalStore, Session, default_db_path};
use reporter::PlaybackReporter;
use session::PlaybackSessionController;
use slint::{Timer, TimerMode};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

slint::include_modules!();

const ASPECTS: [(Option<&str>, &str); 4] = [
    (None, "Auto"),
    (Some("16:9"), "16:9"),
    (Some("4:3"), "4:3"),
    (Some("2.35:1"), "2.35:1"),
];

#[derive(Default)]
struct Options {
    url: Option<String>,
    headers: Vec<String>,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--header" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--header requires a value".to_owned())?;
                    options
                        .headers
                        .push(parse_header(&value).map_err(|error| error.to_string())?);
                }
                "--" => continue,
                _ if argument.starts_with("--header=") => {
                    let value = argument.trim_start_matches("--header=");
                    options
                        .headers
                        .push(parse_header(value).map_err(|error| error.to_string())?);
                }
                _ if argument.starts_with('-') => {
                    return Err(format!("unknown argument: {argument}"));
                }
                _ => {
                    if options.url.is_some() {
                        return Err("only one media URL can be passed".to_owned());
                    }
                    options.url = Some(argument);
                }
            }
        }
        Ok(options)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    slint::BackendSelector::new().require_opengl().select()?;

    let ui = MainWindow::new()?;
    let options = Options::parse(std::env::args().skip(1))?;

    let player = match Player::new() {
        Ok(player) => Some(Rc::new(player)),
        Err(error) => {
            show_error(&ui, error.to_string());
            None
        }
    };

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
    let credential_store = Arc::new(CredentialStore::new());
    let device_id = local_store
        .get_or_create_device_id()
        .unwrap_or_else(|_| "desktop-device-default".to_string());
    let jellyfin = Arc::new(JellyfinClient::new(device_id));
    let resolver = Arc::new(ResolverClient::new());
    let reporter = Arc::new(PlaybackReporter::new(jellyfin.clone()));
    let session_ctrl = Arc::new(PlaybackSessionController::new(local_store.clone()));

    let current_playing_item: Arc<Mutex<Option<TvItem>>> = Arc::new(Mutex::new(None));
    let cached_catalog: Arc<Mutex<Vec<TvItem>>> = Arc::new(Mutex::new(sample_catalog_tv_items()));
    let autoplay_active = Arc::new(AtomicBool::new(false));
    let hwdec_active = Arc::new(AtomicBool::new(true));

    install_controls(
        &ui,
        player.clone(),
        options.headers.clone(),
        autoplay_active.clone(),
        hwdec_active.clone(),
        reporter.clone(),
        session_ctrl.clone(),
        current_playing_item.clone(),
        cached_catalog.clone(),
        credential_store.clone(),
        local_store.clone(),
        jellyfin.clone(),
    );
    install_fullscreen(&ui);
    install_stage2_handlers(
        &ui,
        player.clone(),
        options.headers.clone(),
        local_store.clone(),
        credential_store.clone(),
        jellyfin.clone(),
        resolver.clone(),
        reporter.clone(),
        session_ctrl.clone(),
        current_playing_item.clone(),
        cached_catalog.clone(),
    );

    // Initial session restore & catalog population
    if let Ok(Some(session)) = local_store.load_session() {
        let _ = credential_store.save(session.clone());
        reporter.set_auth(
            Some(session.server_url.clone()),
            Some(session.access_token.clone()),
        );
        ui.set_is_logged_in(true);
        ui.set_logged_in_user(session.username.clone().into());
        ui.set_server_url_input(session.server_url.clone().into());

        let weak = ui.as_weak();
        let jf = jellyfin.clone();
        let catalog_ref = cached_catalog.clone();
        std::thread::spawn(move || {
            let (cards, raw_items) = match jf.get_latest_items(
                &session.server_url,
                &session.user_id,
                &session.access_token,
                None,
                50,
            ) {
                Ok(list) => {
                    let cards = list.iter().map(tv_item_to_card).collect::<Vec<_>>();
                    (cards, list)
                }
                Err(_) => (sample_catalog_items(), sample_catalog_tv_items()),
            };
            *catalog_ref.lock().unwrap() = raw_items;
            slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    set_catalog(&ui, cards);
                }
            })
            .ok();
        });
    } else {
        set_catalog(&ui, sample_catalog_items());
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

    let frame_pending = Arc::new(AtomicBool::new(false));
    if player.is_some() {
        install_renderer(&ui, player.clone().expect("player present"), &frame_pending)?;
    }

    if let (Some(player), Some(initial)) = (&player, initial_media(&options)) {
        if initial.ends_with("sample.mp4") {
            let sample_item = sample_catalog_tv_items()[0].clone();
            start_item_playback(
                &ui,
                player,
                sample_item,
                &options.headers,
                &credential_store,
                &local_store,
                &jellyfin,
                &reporter,
                &session_ctrl,
                &current_playing_item,
            );
        } else {
            load_media(&ui, player, &initial, &options.headers);
            ui.set_url_text(initial.into());
        }
        if options.url.is_some() {
            ui.set_active_screen("player".into());
        }
    } else if player.is_some() {
        ui.set_status("Ready.".into());
    }

    install_polling(
        &ui,
        player.clone(),
        frame_pending,
        reporter.clone(),
        session_ctrl.clone(),
        current_playing_item.clone(),
        cached_catalog.clone(),
        autoplay_active.clone(),
        hwdec_active.clone(),
        options.headers.clone(),
        credential_store.clone(),
        local_store.clone(),
        jellyfin.clone(),
    );

    ui.show()?;
    ui.run()?;
    Ok(())
}

fn initial_media(options: &Options) -> Option<String> {
    if let Some(url) = &options.url {
        return Some(url.clone());
    }
    media_beside_executable("sample.mp4").or_else(|| {
        let local = PathBuf::from("sample.mp4");
        local.exists().then(|| local.to_string_lossy().into_owned())
    })
}

fn media_beside_executable(name: &str) -> Option<String> {
    let executable = std::env::current_exe().ok()?;
    let candidate = executable.parent()?.join(name);
    candidate
        .is_file()
        .then(|| candidate.to_string_lossy().into_owned())
}

fn start_item_playback(
    ui: &MainWindow,
    player: &Player,
    item: TvItem,
    headers: &[String],
    credential_store: &Arc<CredentialStore>,
    local_store: &Arc<LocalStore>,
    _jellyfin: &Arc<JellyfinClient>,
    reporter: &Arc<PlaybackReporter>,
    session_ctrl: &Arc<PlaybackSessionController>,
    current_item: &Arc<Mutex<Option<TvItem>>>,
) {
    let _req_id = session_ctrl.next_request_id();

    // 1. Persist outgoing video's progress and stop session
    if let Some(prev) = current_item.lock().unwrap().take() {
        let pos = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
        let dur = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
        session_ctrl.checkpoint_progress(&prev, pos, dur, false);
        reporter.stop_session(pos);
    }

    // 2. Resolve stream URL
    let session = credential_store.get().ok().flatten();
    let resolver_url = local_store
        .get_setting("resolver_url")
        .ok()
        .flatten()
        .or_else(|| session.as_ref().map(|s| s.server_url.clone()));

    let (stream_url, play_method) = match PlaybackSessionController::resolve_stream_url(
        &item,
        session.as_ref().map(|s| s.server_url.as_str()),
        resolver_url.as_deref(),
        session.as_ref().map(|s| s.access_token.as_str()),
    ) {
        Ok(res) => res,
        Err(err) => {
            show_error(ui, err);
            return;
        }
    };

    let mut stream_headers = headers.to_vec();
    if !item.is_telegram() {
        if let Some(sess) = &session {
            if !sess.access_token.is_empty() {
                stream_headers.push(format!("X-Emby-Token: {}", sess.access_token));
            }
        }
    }

    // 3. Determine resume position
    let duration_ms = item.runtime_ticks.map(|t| (t / 10_000) as u64).unwrap_or(0);
    let mut start_pos_secs = 0.0;
    ui.set_resume_prompt_visible(false);

    if item.is_telegram() {
        let prov_id = item
            .provider_video_id
            .as_deref()
            .or_else(|| item.id.strip_prefix("telegram:"))
            .unwrap_or(&item.id);
        if let Some(resume_ms) = session_ctrl.get_telegram_resume_position(prov_id) {
            if PlaybackSessionController::should_prompt_resume(resume_ms, duration_ms) {
                ui.set_resume_prompt_position((resume_ms as f32) / 1000.0);
                ui.set_resume_prompt_time(format_time(resume_ms as f64 / 1000.0).into());
                ui.set_resume_prompt_visible(true);
            }
        }
    } else {
        let resume_ms = (item.playback_position_ticks / 10_000).max(0) as u64;
        if PlaybackSessionController::should_prompt_resume(resume_ms, duration_ms) {
            start_pos_secs = resume_ms as f64 / 1000.0;
        }
    }

    // 4. Update UI & current item
    *current_item.lock().unwrap() = Some(item.clone());
    ui.set_url_text(stream_url.clone().into());
    ui.set_status(format!("Loading: {}", item.title).into());
    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
    ui.set_active_screen("player".into());

    // 5. Start player & session reporting
    let start_pos_opt = if start_pos_secs > 0.0 {
        Some(start_pos_secs)
    } else {
        None
    };
    let path = resolve_media_input(&stream_url);
    if let Err(e) = player.load_with_start(&path, &stream_headers, start_pos_opt) {
        show_error(ui, e.to_string());
        return;
    }

    reporter.start_session(
        &item.id,
        item.media_source_id.clone(),
        None,
        play_method,
        (start_pos_secs * 1000.0) as u64,
    );
}

fn play_random_catalog_item(
    ui: &MainWindow,
    player: &Player,
    headers: &[String],
    credential_store: &Arc<CredentialStore>,
    local_store: &Arc<LocalStore>,
    jellyfin: &Arc<JellyfinClient>,
    reporter: &Arc<PlaybackReporter>,
    session_ctrl: &Arc<PlaybackSessionController>,
    current_item: &Arc<Mutex<Option<TvItem>>>,
    cached_catalog: &Arc<Mutex<Vec<TvItem>>>,
) {
    let items = cached_catalog.lock().unwrap().clone();
    if items.is_empty() {
        show_error(ui, "No catalog items available for random playback");
        return;
    }
    let current_id = current_item
        .lock()
        .unwrap()
        .as_ref()
        .map(|i| i.id.clone())
        .unwrap_or_default();
    let candidates: Vec<&TvItem> = items.iter().filter(|i| i.id != current_id).collect();
    let target = if !candidates.is_empty() {
        let idx = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_millis() as usize
            % candidates.len();
        candidates[idx].clone()
    } else {
        items[0].clone()
    };

    start_item_playback(
        ui,
        player,
        target,
        headers,
        credential_store,
        local_store,
        jellyfin,
        reporter,
        session_ctrl,
        current_item,
    );
}

fn install_controls(
    ui: &MainWindow,
    player: Option<Rc<Player>>,
    headers: Vec<String>,
    autoplay_active: Arc<AtomicBool>,
    hwdec_active: Arc<AtomicBool>,
    reporter: Arc<PlaybackReporter>,
    session_ctrl: Arc<PlaybackSessionController>,
    current_item: Arc<Mutex<Option<TvItem>>>,
    cached_catalog: Arc<Mutex<Vec<TvItem>>>,
    credential_store: Arc<CredentialStore>,
    local_store: Arc<LocalStore>,
    jellyfin: Arc<JellyfinClient>,
) {
    {
        let weak = ui.as_weak();
        let player = player.clone();
        let reporter = reporter.clone();
        ui.on_toggle_play(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else {
                show_error(&ui, "libmpv is not available");
                return;
            };
            match player.toggle_pause() {
                Ok(paused) => {
                    ui.set_playing(!paused);
                    let pos_ms = (player.position().unwrap_or(0.0) * 1000.0) as u64;
                    reporter.report_progress_now(pos_ms, paused);
                }
                Err(error) => show_error(&ui, error.to_string()),
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let reporter = reporter.clone();
        ui.on_seek_to(move |seconds| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            if let Err(error) = player.seek_absolute(seconds as f64) {
                show_error(&ui, error.to_string());
            } else {
                let pos_ms = (seconds as f64 * 1000.0) as u64;
                reporter.report_progress_now(pos_ms, player.is_paused());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let reporter = reporter.clone();
        ui.on_seek_relative(move |seconds| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            if let Err(error) = player.seek_relative(seconds as f64) {
                show_error(&ui, error.to_string());
            } else {
                let pos_ms = (player.position().unwrap_or(0.0) * 1000.0) as u64;
                reporter.report_progress_now(pos_ms, player.is_paused());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_set_volume(move |volume| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            if let Err(error) = player.set_volume(volume as f64) {
                show_error(&ui, error.to_string());
            }
            if (volume - ui.get_volume()).abs() > 0.5 {
                ui.set_volume(volume);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let headers = headers.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        ui.on_load_url(move |text| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else {
                show_error(&ui, "libmpv is not available");
                return;
            };
            let text = text.to_string();
            if text.trim().is_empty() {
                show_error(&ui, "Enter a media URL or file path first");
                return;
            }
            let custom_item = TvItem {
                id: text.clone(),
                title: Path::new(&text)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Media".into()),
                overview: None,
                year: None,
                runtime_ticks: None,
                genres: vec![],
                tags: vec![],
                studios: vec![],
                people: vec![],
                primary_image_tag: None,
                backdrop_image_tag: None,
                thumb_image_tag: None,
                is_favorite: false,
                playback_position_ticks: 0,
                played_percentage: None,
                is_played: false,
                provider_video_id: if text.starts_with("telegram:") {
                    Some(text.replace("telegram:", ""))
                } else {
                    None
                },
                media_source_id: None,
                container: None,
                file_size_bytes: None,
                width: None,
                height: None,
            };
            start_item_playback(
                &ui,
                player,
                custom_item,
                &headers,
                &credential_store,
                &local_store,
                &jellyfin,
                &reporter,
                &session_ctrl,
                &current_item,
            );
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let headers = headers.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        ui.on_load_sample(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let sample_item = sample_catalog_tv_items()[0].clone();
            start_item_playback(
                &ui,
                player,
                sample_item,
                &headers,
                &credential_store,
                &local_store,
                &jellyfin,
                &reporter,
                &session_ctrl,
                &current_item,
            );
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let headers = headers.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        ui.on_open_file(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            match pick_media_file() {
                Some(path) => {
                    let local_item = TvItem {
                        id: path.clone(),
                        title: Path::new(&path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "Local Video".into()),
                        overview: None,
                        year: None,
                        runtime_ticks: None,
                        genres: vec![],
                        tags: vec![],
                        studios: vec![],
                        people: vec![],
                        primary_image_tag: None,
                        backdrop_image_tag: None,
                        thumb_image_tag: None,
                        is_favorite: false,
                        playback_position_ticks: 0,
                        played_percentage: None,
                        is_played: false,
                        provider_video_id: None,
                        media_source_id: None,
                        container: None,
                        file_size_bytes: None,
                        width: None,
                        height: None,
                    };
                    start_item_playback(
                        &ui,
                        player,
                        local_item,
                        &headers,
                        &credential_store,
                        &local_store,
                        &jellyfin,
                        &reporter,
                        &session_ctrl,
                        &current_item,
                    );
                }
                None => {
                    if !cfg!(windows) {
                        ui.set_status(
                            "Native file dialog is only enabled in the Windows prototype; \
                             enter a path or URL instead."
                                .into(),
                        );
                        ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
                    }
                }
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let index = Rc::new(Cell::new(0usize));
        ui.on_cycle_aspect(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let next = (index.get() + 1) % ASPECTS.len();
            index.set(next);
            let (aspect, label) = ASPECTS[next];
            if let Err(error) = player.set_aspect_override(aspect) {
                show_error(&ui, error.to_string());
                return;
            }
            ui.set_aspect_label(format!("Aspect: {label}").into());
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_toggle_hud(move || {
            let Some(ui) = weak.upgrade() else { return };
            let visible = !ui.get_hud_visible();
            ui.set_hud_visible(visible);
            if !visible {
                ui.set_hud_text("".into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_toggle_mute(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let muted = !player.is_muted();
            if let Err(error) = player.set_muted(muted) {
                show_error(&ui, error.to_string());
                return;
            }
            ui.set_muted(muted);
        });
    }

    // Stage 3 Playback Controls
    {
        let weak = ui.as_weak();
        let player = player.clone();
        const SPEEDS: [f64; 4] = [1.0, 1.25, 1.5, 2.0];
        let speed_idx = Rc::new(Cell::new(0usize));
        ui.on_cycle_speed(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let next = (speed_idx.get() + 1) % SPEEDS.len();
            speed_idx.set(next);
            let speed = SPEEDS[next];
            if let Err(error) = player.set_speed(speed) {
                show_error(&ui, error.to_string());
            } else {
                ui.set_speed_label(format!("{speed:.2}x").into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        const PROFILES: [BufferProfile; 4] = [
            BufferProfile::Standard,
            BufferProfile::FastStart,
            BufferProfile::Large,
            BufferProfile::LowMemory,
        ];
        let prof_idx = Rc::new(Cell::new(0usize));
        ui.on_cycle_buffer_profile(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let next = (prof_idx.get() + 1) % PROFILES.len();
            prof_idx.set(next);
            let profile = PROFILES[next];
            if let Err(error) = player.set_buffer_profile(profile) {
                show_error(&ui, error.to_string());
            } else {
                ui.set_buffer_profile_label(format!("Buffer: {}", profile.name()).into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let hwdec_active = hwdec_active.clone();
        ui.on_toggle_hwdec(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let active = !hwdec_active.load(Ordering::SeqCst);
            hwdec_active.store(active, Ordering::SeqCst);
            let mode = if active { "auto-safe" } else { "no" };
            if let Err(error) = player.set_hwdec(mode) {
                show_error(&ui, error.to_string());
            } else {
                ui.set_hwdec_active(active);
                ui.set_status(format!("Decoder mode: {mode}").into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let autoplay_active = autoplay_active.clone();
        ui.on_toggle_autoplay(move || {
            let Some(ui) = weak.upgrade() else { return };
            let active = !autoplay_active.load(Ordering::SeqCst);
            autoplay_active.store(active, Ordering::SeqCst);
            ui.set_autoplay_active(active);
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        let headers = headers.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let cached_catalog = cached_catalog.clone();
        ui.on_play_random(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            play_random_catalog_item(
                &ui,
                player,
                &headers,
                &credential_store,
                &local_store,
                &jellyfin,
                &reporter,
                &session_ctrl,
                &current_item,
                &cached_catalog,
            );
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_accept_resume(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            let pos = ui.get_resume_prompt_position();
            ui.set_resume_prompt_visible(false);
            let _ = player.seek_absolute(pos as f64);
        });
    }

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_reject_resume(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else { return };
            ui.set_resume_prompt_visible(false);
            let _ = player.seek_absolute(0.0);
        });
    }
}

fn install_fullscreen(ui: &MainWindow) {
    let fullscreen = Rc::new(Cell::new(false));

    {
        let ui_weak = ui.as_weak();
        let fullscreen = fullscreen.clone();
        ui.on_toggle_fullscreen(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let next = !fullscreen.get();
            ui.window().set_fullscreen(next);
            fullscreen.set(next);
        });
    }

    {
        let ui_weak = ui.as_weak();
        let fullscreen = fullscreen.clone();
        ui.on_escape_pressed(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            if fullscreen.get() {
                ui.window().set_fullscreen(false);
                fullscreen.set(false);
            } else if ui.get_active_screen() != "browse" {
                ui.set_active_screen("browse".into());
            }
        });
    }
}

fn sample_catalog_tv_items() -> Vec<TvItem> {
    vec![
        TvItem {
            id: "sample".into(),
            title: "Local Demo Sample (1080p)".into(),
            overview: Some("Local MP4 test sample".into()),
            year: Some(2026),
            runtime_ticks: Some(100_000_000), // 10s
            genres: vec!["Sample".into()],
            tags: vec![],
            studios: vec!["Local".into()],
            people: vec![],
            primary_image_tag: None,
            backdrop_image_tag: None,
            thumb_image_tag: None,
            is_favorite: false,
            playback_position_ticks: 0,
            played_percentage: None,
            is_played: false,
            provider_video_id: None,
            media_source_id: None,
            container: Some("mp4".into()),
            file_size_bytes: Some(120_000),
            width: Some(1920),
            height: Some(1080),
        },
        TvItem {
            id: "telegram:demo_channel:101".into(),
            title: "Channel Highlight #101".into(),
            overview: Some("Exclusive channel video".into()),
            year: Some(2026),
            runtime_ticks: Some(8_620_000_000), // 14m 22s = 862s
            genres: vec!["Channel".into()],
            tags: vec![],
            studios: vec!["Channel Studios".into()],
            people: vec![],
            primary_image_tag: None,
            backdrop_image_tag: None,
            thumb_image_tag: None,
            is_favorite: false,
            playback_position_ticks: 0,
            played_percentage: None,
            is_played: false,
            provider_video_id: Some("demo_channel:101".into()),
            media_source_id: None,
            container: Some("mp4".into()),
            file_size_bytes: Some(450_000_000),
            width: Some(1920),
            height: Some(1080),
        },
        TvItem {
            id: "telegram:demo_channel:102".into(),
            title: "Exclusive Scene #102".into(),
            overview: Some("Scene clip".into()),
            year: Some(2026),
            runtime_ticks: Some(15_400_000_000), // 25m 40s
            genres: vec!["Exclusive".into()],
            tags: vec![],
            studios: vec!["Channel Studios".into()],
            people: vec![],
            primary_image_tag: None,
            backdrop_image_tag: None,
            thumb_image_tag: None,
            is_favorite: true,
            playback_position_ticks: 0,
            played_percentage: None,
            is_played: true,
            provider_video_id: Some("demo_channel:102".into()),
            media_source_id: None,
            container: Some("mp4".into()),
            file_size_bytes: Some(850_000_000),
            width: Some(1920),
            height: Some(1080),
        },
    ]
}

fn sample_catalog_items() -> Vec<VideoCardData> {
    sample_catalog_tv_items()
        .iter()
        .map(tv_item_to_card)
        .collect()
}

fn sample_channel_items() -> Vec<VideoCardData> {
    vec![
        VideoCardData {
            id: "telegram:channel_4k:1".into(),
            title: "4K Studios Channel - Top Clips".into(),
            duration: "32:15".into(),
            year: "2026".into(),
            is_played: false,
            provider: "telegram".into(),
        },
        VideoCardData {
            id: "telegram:channel_vr:2".into(),
            title: "VR Scenes Channel - Volume 3".into(),
            duration: "45:00".into(),
            year: "2026".into(),
            is_played: false,
            provider: "telegram".into(),
        },
    ]
}

fn set_catalog(ui: &MainWindow, items: Vec<VideoCardData>) {
    let model = std::rc::Rc::new(slint::VecModel::from(items));
    ui.set_catalog_items(model.into());
}

fn set_discovered_servers(ui: &MainWindow, items: Vec<DiscoveredServerItem>) {
    let model = std::rc::Rc::new(slint::VecModel::from(items));
    ui.set_discovered_servers(model.into());
}

fn tv_item_to_card(item: &TvItem) -> VideoCardData {
    let year_str = item.year.map(|y| y.to_string()).unwrap_or_default();
    let duration_str = item.formatted_duration();
    let provider = if item.is_telegram() {
        "telegram".to_string()
    } else if item.id == "sample" || item.id.starts_with('/') || item.id.contains(":\\") {
        "local".to_string()
    } else {
        "jellyfin".to_string()
    };
    VideoCardData {
        id: item.id.clone().into(),
        title: item.title.clone().into(),
        duration: duration_str.into(),
        year: year_str.into(),
        is_played: item.is_played,
        provider: provider.into(),
    }
}

fn install_stage2_handlers(
    ui: &MainWindow,
    player: Option<Rc<Player>>,
    headers: Vec<String>,
    local_store: Arc<LocalStore>,
    credential_store: Arc<CredentialStore>,
    jellyfin: Arc<JellyfinClient>,
    resolver: Arc<ResolverClient>,
    reporter: Arc<PlaybackReporter>,
    session_ctrl: Arc<PlaybackSessionController>,
    current_item: Arc<Mutex<Option<TvItem>>>,
    cached_catalog: Arc<Mutex<Vec<TvItem>>>,
) {
    // 1. Screen switching
    {
        let weak = ui.as_weak();
        ui.on_switch_screen(move |screen| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_active_screen(screen);
        });
    }

    // 2. Perform Login
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let reporter = reporter.clone();
        let cached_catalog = cached_catalog.clone();
        ui.on_perform_login(move |server_url, username, password| {
            let Some(ui) = weak.upgrade() else { return };
            let server_url = server_url.trim().to_string();
            let username = username.trim().to_string();
            let password = password.to_string();

            if server_url.is_empty() {
                show_error(&ui, "Server address cannot be empty");
                return;
            }

            match ServiceUrlPolicy::inspect(&server_url, false) {
                ServiceUrlValidation::Valid(service_url) => {
                    let normalized = service_url.normalized;
                    let weak = weak.clone();
                    let jellyfin = jellyfin.clone();
                    let local_store = local_store.clone();
                    let credential_store = credential_store.clone();
                    let reporter = reporter.clone();
                    let cached_catalog = cached_catalog.clone();
                    ui.set_status("Connecting to server...".into());
                    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));

                    std::thread::spawn(move || {
                        let public_info = jellyfin.get_public_system_info(&normalized).ok();
                        match jellyfin.authenticate_by_name(&normalized, &username, &password) {
                            Ok(auth) => {
                                let session = Session {
                                    server_url: normalized.clone(),
                                    user_id: auth.user_id.clone(),
                                    username: auth.username.clone(),
                                    access_token: auth.access_token.clone(),
                                    server_name: public_info
                                        .map(|p| p.name)
                                        .unwrap_or_else(|| "Jellyfin".into()),
                                };
                                let _ = local_store.save_session(&session);
                                let _ = credential_store.save(session.clone());
                                reporter.set_auth(
                                    Some(session.server_url.clone()),
                                    Some(session.access_token.clone()),
                                );

                                let (cards, raw_items) = match jellyfin.get_latest_items(
                                    &normalized,
                                    &session.user_id,
                                    &session.access_token,
                                    None,
                                    50,
                                ) {
                                    Ok(list) => {
                                        let cards =
                                            list.iter().map(tv_item_to_card).collect::<Vec<_>>();
                                        (cards, list)
                                    }
                                    Err(_) => (sample_catalog_items(), sample_catalog_tv_items()),
                                };
                                *cached_catalog.lock().unwrap() = raw_items;

                                slint::invoke_from_event_loop(move || {
                                    let Some(ui) = weak.upgrade() else { return };
                                    ui.set_is_logged_in(true);
                                    ui.set_logged_in_user(session.username.into());
                                    ui.set_server_url_input(session.server_url.into());
                                    set_catalog(&ui, cards);
                                    ui.set_status("Logged in successfully".into());
                                    ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                                    ui.set_active_screen("browse".into());
                                })
                                .ok();
                            }
                            Err(error) => {
                                slint::invoke_from_event_loop(move || {
                                    let Some(ui) = weak.upgrade() else { return };
                                    show_error(&ui, format!("Login failed: {error}"));
                                })
                                .ok();
                            }
                        }
                    });
                }
                ServiceUrlValidation::Invalid(err) => {
                    show_error(&ui, err);
                }
            }
        });
    }

    // 3. Perform Logout
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let reporter = reporter.clone();
        let cached_catalog = cached_catalog.clone();
        ui.on_perform_logout(move || {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.clear_session();
            let _ = credential_store.clear();
            reporter.set_auth(None, None);
            *cached_catalog.lock().unwrap() = sample_catalog_tv_items();
            ui.set_is_logged_in(false);
            ui.set_logged_in_user("".into());
            set_catalog(&ui, sample_catalog_items());
            ui.set_status("Logged out".into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
        });
    }

    // 4. Quick Connect
    let qc_running = Arc::new(AtomicBool::new(false));
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let reporter = reporter.clone();
        let cached_catalog = cached_catalog.clone();
        let qc_running = qc_running.clone();
        ui.on_start_quick_connect(move || {
            let Some(ui) = weak.upgrade() else { return };
            let server_url = ui.get_server_url_input().trim().to_string();
            if server_url.is_empty() {
                show_error(&ui, "Server address cannot be empty");
                return;
            }
            let normalized = match ServiceUrlPolicy::inspect(&server_url, false) {
                ServiceUrlValidation::Valid(service_url) => service_url.normalized,
                ServiceUrlValidation::Invalid(err) => {
                    show_error(&ui, err);
                    return;
                }
            };

            qc_running.store(true, Ordering::SeqCst);
            let qc_running = qc_running.clone();
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let local_store = local_store.clone();
            let credential_store = credential_store.clone();
            let reporter = reporter.clone();
            let cached_catalog = cached_catalog.clone();

            ui.set_status("Requesting Quick Connect code...".into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));

            std::thread::spawn(move || {
                let init = match jellyfin.quick_connect_initiate(&normalized) {
                    Ok(init) => init,
                    Err(err) => {
                        let w = weak.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Some(ui) = w.upgrade() {
                                show_error(&ui, format!("Quick Connect failed: {err}"));
                            }
                        })
                        .ok();
                        return;
                    }
                };

                let code = init.code.clone();
                let secret = init.secret.clone();
                let w = weak.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = w.upgrade() {
                        ui.set_quick_connect_code(code.into());
                        ui.set_status("Enter code on another device to authorize".into());
                    }
                })
                .ok();

                for _ in 0..60 {
                    std::thread::sleep(Duration::from_secs(2));
                    if !qc_running.load(Ordering::SeqCst) {
                        return;
                    }
                    if let Ok(status) = jellyfin.quick_connect_poll(&normalized, &secret) {
                        if status.authenticated {
                            match jellyfin.quick_connect_authenticate(&normalized, &secret) {
                                Ok(auth) => {
                                    let public_info =
                                        jellyfin.get_public_system_info(&normalized).ok();
                                    let session = Session {
                                        server_url: normalized.clone(),
                                        user_id: auth.user_id.clone(),
                                        username: auth.username.clone(),
                                        access_token: auth.access_token.clone(),
                                        server_name: public_info
                                            .map(|p| p.name)
                                            .unwrap_or_else(|| "Jellyfin".into()),
                                    };
                                    let _ = local_store.save_session(&session);
                                    let _ = credential_store.save(session.clone());
                                    reporter.set_auth(
                                        Some(session.server_url.clone()),
                                        Some(session.access_token.clone()),
                                    );

                                    let (cards, raw_items) = match jellyfin.get_latest_items(
                                        &normalized,
                                        &session.user_id,
                                        &session.access_token,
                                        None,
                                        50,
                                    ) {
                                        Ok(list) => {
                                            let cards = list
                                                .iter()
                                                .map(tv_item_to_card)
                                                .collect::<Vec<_>>();
                                            (cards, list)
                                        }
                                        Err(_) => {
                                            (sample_catalog_items(), sample_catalog_tv_items())
                                        }
                                    };
                                    *cached_catalog.lock().unwrap() = raw_items;

                                    let w = weak.clone();
                                    slint::invoke_from_event_loop(move || {
                                        if let Some(ui) = w.upgrade() {
                                            ui.set_quick_connect_code("".into());
                                            ui.set_is_logged_in(true);
                                            ui.set_logged_in_user(session.username.into());
                                            set_catalog(&ui, cards);
                                            ui.set_status("Quick Connect authenticated!".into());
                                            ui.set_status_color(slint::Color::from_rgb_u8(
                                                140, 200, 160,
                                            ));
                                            ui.set_active_screen("browse".into());
                                        }
                                    })
                                    .ok();
                                    return;
                                }
                                Err(e) => {
                                    let w = weak.clone();
                                    slint::invoke_from_event_loop(move || {
                                        if let Some(ui) = w.upgrade() {
                                            show_error(
                                                &ui,
                                                format!("Quick Connect auth error: {e}"),
                                            );
                                        }
                                    })
                                    .ok();
                                    return;
                                }
                            }
                        }
                    }
                }

                let w = weak.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = w.upgrade() {
                        ui.set_quick_connect_code("".into());
                        show_error(&ui, "Quick Connect timed out");
                    }
                })
                .ok();
            });
        });
    }

    {
        let weak = ui.as_weak();
        let qc_running = qc_running.clone();
        ui.on_cancel_quick_connect(move || {
            let Some(ui) = weak.upgrade() else { return };
            qc_running.store(false, Ordering::SeqCst);
            ui.set_quick_connect_code("".into());
            ui.set_status("Quick Connect cancelled".into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
        });
    }

    // 4b. Discovered Servers
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        ui.on_select_discovered_server(move |server_url| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_server_url_input(server_url.clone());
            let server_url = server_url.trim().to_string();
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            std::thread::spawn(move || {
                if let Ok(info) = jellyfin.get_public_system_info(&server_url) {
                    slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_status(format!("Selected server: {} ({})", info.name, info.url).into());
                            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                        }
                    })
                    .ok();
                }
            });
        });
    }

    let is_scanning = Arc::new(AtomicBool::new(false));
    {
        let weak = ui.as_weak();
        let is_scanning = is_scanning.clone();
        ui.on_scan_servers(move || {
            let Some(ui) = weak.upgrade() else { return };
            if is_scanning.swap(true, Ordering::SeqCst) {
                return;
            }
            ui.set_is_scanning_servers(true);
            ui.set_status("Scanning local network for Jellyfin servers...".into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));

            let weak = weak.clone();
            let is_scanning = is_scanning.clone();
            std::thread::spawn(move || {
                let servers = discover_servers(Duration::from_millis(2000));
                is_scanning.store(false, Ordering::SeqCst);
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    ui.set_is_scanning_servers(false);
                    let count = servers.len();
                    let items = servers
                        .iter()
                        .map(|s| DiscoveredServerItem {
                            id: s.id.clone().into(),
                            name: s.name.clone().into(),
                            url: s.url.clone().into(),
                        })
                        .collect::<Vec<_>>();
                    set_discovered_servers(&ui, items);
                    if count > 0 {
                        ui.set_status(format!("Discovered {count} server(s) on network").into());
                        ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                    } else {
                        ui.set_status("No local Jellyfin servers found on network".into());
                        ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
                    }
                })
                .ok();
            });
        });
    }

    // 5. Search Catalog
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let cached_catalog = cached_catalog.clone();
        ui.on_search_catalog(move |query| {
            if weak.upgrade().is_none() {
                return;
            }
            let query = query.trim().to_string();
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let credential_store = credential_store.clone();
            let local_store = local_store.clone();
            let cached_catalog = cached_catalog.clone();

            std::thread::spawn(move || {
                if !query.is_empty() {
                    let _ = local_store.add_search_query(&query);
                }

                let session = credential_store.get().ok().flatten();
                let (cards, raw_items) = if let Some(session) = session {
                    if query.is_empty() {
                        jellyfin
                            .get_latest_items(
                                &session.server_url,
                                &session.user_id,
                                &session.access_token,
                                None,
                                50,
                            )
                            .map(|list| (list.iter().map(tv_item_to_card).collect(), list))
                            .unwrap_or_else(|_| (sample_catalog_items(), sample_catalog_tv_items()))
                    } else {
                        let q = GetItemsQuery {
                            parent_id: None,
                            start_index: 0,
                            limit: 50,
                            sort_by: "SortName",
                            sort_descending: false,
                            search_term: Some(&query),
                        };
                        jellyfin
                            .get_items(
                                &session.server_url,
                                &session.user_id,
                                &session.access_token,
                                &q,
                            )
                            .map(|page| {
                                (
                                    page.items.iter().map(tv_item_to_card).collect(),
                                    page.items,
                                )
                            })
                            .unwrap_or_default()
                    }
                } else if query.is_empty() {
                    (sample_catalog_items(), sample_catalog_tv_items())
                } else {
                    let filtered: Vec<TvItem> = sample_catalog_tv_items()
                        .into_iter()
                        .filter(|item| item.title.to_lowercase().contains(&query.to_lowercase()))
                        .collect();
                    let cards = filtered.iter().map(tv_item_to_card).collect();
                    (cards, filtered)
                };

                *cached_catalog.lock().unwrap() = raw_items;

                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        set_catalog(&ui, cards);
                    }
                })
                .ok();
            });
        });
    }

    // 6. Filter Category
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let cached_catalog = cached_catalog.clone();
        let resolver = resolver.clone();
        ui.on_filter_category(move |cat| {
            if weak.upgrade().is_none() {
                return;
            }
            let cat = cat.to_string();
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let credential_store = credential_store.clone();
            let local_store = local_store.clone();
            let cached_catalog = cached_catalog.clone();
            let _resolver = resolver.clone();

            std::thread::spawn(move || {
                let session = credential_store.get().ok().flatten();
                let (cards, raw_items) = match cat.as_str() {
                    "Latest" => {
                        if let Some(session) = session {
                            jellyfin
                                .get_latest_items(
                                    &session.server_url,
                                    &session.user_id,
                                    &session.access_token,
                                    None,
                                    50,
                                )
                                .map(|list| (list.iter().map(tv_item_to_card).collect(), list))
                                .unwrap_or_else(|_| {
                                    (sample_catalog_items(), sample_catalog_tv_items())
                                })
                        } else {
                            (sample_catalog_items(), sample_catalog_tv_items())
                        }
                    }
                    "Resume" => {
                        let mut resume_items = if let Some(session) = &session {
                            jellyfin
                                .get_resume_items(
                                    &session.server_url,
                                    &session.user_id,
                                    &session.access_token,
                                    50,
                                )
                                .unwrap_or_default()
                        } else {
                            Vec::new()
                        };

                        // Merge local Telegram progress records
                        if let Ok(records) = local_store.get_all_telegram_progress(50) {
                            for rec in records {
                                if rec.is_played {
                                    continue;
                                }
                                let rec_id = format!("telegram:{}", rec.provider_video_id);
                                if !resume_items.iter().any(|i| {
                                    i.id == rec_id
                                        || i.provider_video_id.as_deref()
                                            == Some(&rec.provider_video_id)
                                }) {
                                    resume_items.push(TvItem {
                                        id: rec_id.clone(),
                                        title: if rec.title.is_empty() {
                                            format!("Telegram {}", rec.provider_video_id)
                                        } else {
                                            rec.title.clone()
                                        },
                                        overview: None,
                                        year: None,
                                        runtime_ticks: Some((rec.duration_ms * 10_000) as i64),
                                        genres: vec!["Telegram".into()],
                                        tags: vec![],
                                        studios: vec![],
                                        people: vec![],
                                        primary_image_tag: None,
                                        backdrop_image_tag: None,
                                        thumb_image_tag: None,
                                        is_favorite: false,
                                        playback_position_ticks: (rec.position_ms * 10_000) as i64,
                                        played_percentage: if rec.duration_ms > 0 {
                                            Some(
                                                (rec.position_ms as f64 / rec.duration_ms as f64)
                                                    * 100.0,
                                            )
                                        } else {
                                            None
                                        },
                                        is_played: false,
                                        provider_video_id: Some(rec.provider_video_id),
                                        media_source_id: None,
                                        container: Some("mp4".into()),
                                        file_size_bytes: None,
                                        width: None,
                                        height: None,
                                    });
                                }
                            }
                        }

                        if resume_items.is_empty() && session.is_none() {
                            (sample_catalog_items(), sample_catalog_tv_items())
                        } else {
                            let cards = resume_items.iter().map(tv_item_to_card).collect();
                            (cards, resume_items)
                        }
                    }
                    "Channels" => {
                        let cards = sample_channel_items();
                        let raw = sample_catalog_tv_items()
                            .into_iter()
                            .filter(|i| i.is_telegram())
                            .collect();
                        (cards, raw)
                    }
                    _ => (sample_catalog_items(), sample_catalog_tv_items()),
                };

                *cached_catalog.lock().unwrap() = raw_items;

                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        set_catalog(&ui, cards);
                    }
                })
                .ok();
            });
        });
    }

    // 7. Play Item
    {
        let weak = ui.as_weak();
        let player = player.clone();
        let headers = headers.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let cached_catalog = cached_catalog.clone();
        ui.on_play_item(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else {
                show_error(&ui, "libmpv is not available");
                return;
            };
            let id = id.to_string();

            // Locate in cached catalog or synthesize TvItem
            let target_item = cached_catalog
                .lock()
                .unwrap()
                .iter()
                .find(|i| i.id == id)
                .cloned()
                .unwrap_or_else(|| {
                    if id == "sample" {
                        sample_catalog_tv_items()[0].clone()
                    } else if id.starts_with("telegram:") {
                        let prov_id = id.strip_prefix("telegram:").unwrap_or(&id).to_string();
                        TvItem {
                            id: id.clone(),
                            title: format!("Telegram video {prov_id}"),
                            overview: None,
                            year: None,
                            runtime_ticks: None,
                            genres: vec!["Telegram".into()],
                            tags: vec![],
                            studios: vec![],
                            people: vec![],
                            primary_image_tag: None,
                            backdrop_image_tag: None,
                            thumb_image_tag: None,
                            is_favorite: false,
                            playback_position_ticks: 0,
                            played_percentage: None,
                            is_played: false,
                            provider_video_id: Some(prov_id),
                            media_source_id: None,
                            container: Some("mp4".into()),
                            file_size_bytes: None,
                            width: None,
                            height: None,
                        }
                    } else {
                        TvItem {
                            id: id.clone(),
                            title: Path::new(&id)
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| id.clone()),
                            overview: None,
                            year: None,
                            runtime_ticks: None,
                            genres: vec![],
                            tags: vec![],
                            studios: vec![],
                            people: vec![],
                            primary_image_tag: None,
                            backdrop_image_tag: None,
                            thumb_image_tag: None,
                            is_favorite: false,
                            playback_position_ticks: 0,
                            played_percentage: None,
                            is_played: false,
                            provider_video_id: None,
                            media_source_id: None,
                            container: None,
                            file_size_bytes: None,
                            width: None,
                            height: None,
                        }
                    }
                });

            start_item_playback(
                &ui,
                player,
                target_item,
                &headers,
                &credential_store,
                &local_store,
                &jellyfin,
                &reporter,
                &session_ctrl,
                &current_item,
            );
        });
    }

    // 8. Panic Exit (Emergency Hide)
    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_panic_exit(move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.window().hide();
            }
            if let Some(player) = &player {
                let _ = player.stop();
                let _ = player.set_muted(true);
            }
            std::process::exit(0);
        });
    }

    // 9. Clear Search History
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_clear_search_history(move || {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.clear_search_history();
            ui.set_status("Search history cleared".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 10. Clear Watch History & Progress
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let cached_catalog = cached_catalog.clone();
        ui.on_clear_progress_history(move || {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.clear_all_progress();
            ui.set_status("Watch history & progress cleared".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            if ui.get_active_category() == "Resume" {
                set_catalog(&ui, sample_catalog_items());
                *cached_catalog.lock().unwrap() = sample_catalog_tv_items();
            }
        });
    }
}

fn install_renderer(
    ui: &MainWindow,
    player: Rc<Player>,
    frame_pending: &Arc<AtomicBool>,
) -> Result<(), slint::SetRenderingNotifierError> {
    let ui_weak = ui.as_weak();
    let frame_pending = frame_pending.clone();
    let flip_y = std::env::var("ADULT_HUB_FLIP_Y")
        .map(|value| value == "1" || value == "true")
        .unwrap_or(false);
    let mut renderer: Option<renderer::Renderer> = None;

    ui.window()
        .set_rendering_notifier(move |state, graphics_api| match state {
            slint::RenderingState::RenderingSetup => {
                let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else {
                    if let Some(ui) = ui_weak.upgrade() {
                        show_error(&ui, "the selected Slint renderer is not OpenGL");
                    }
                    return;
                };
                let get_proc_address: &dyn Fn(&std::ffi::CStr) -> *const std::ffi::c_void =
                    *get_proc_address;
                match renderer::Renderer::new(Rc::clone(&player), get_proc_address, flip_y) {
                    Ok(mut created) => {
                        let pending = frame_pending.clone();
                        let w = ui_weak.clone();
                        created.set_update_callback(move || {
                            if !pending.swap(true, Ordering::SeqCst) {
                                let w = w.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = w.upgrade() {
                                        ui.window().request_redraw();
                                    }
                                });
                            }
                        });
                        renderer = Some(created);
                    }
                    Err(error) => {
                        if let Some(ui) = ui_weak.upgrade() {
                            show_error(&ui, error);
                        }
                    }
                }
            }
            slint::RenderingState::BeforeRendering => {
                frame_pending.store(false, Ordering::SeqCst);
                let Some(renderer) = renderer.as_mut() else {
                    return;
                };
                let Some(ui) = ui_weak.upgrade() else { return };
                let width = ui.get_texture_width().max(2) as u32;
                let height = ui.get_texture_height().max(2) as u32;
                match renderer.render(width, height) {
                    Ok(Some(image)) => ui.set_video_image(image),
                    Ok(None) => {}
                    Err(error) => show_error(&ui, error),
                }
            }
            slint::RenderingState::RenderingTeardown => {
                renderer = None;
            }
            _ => {}
        })
}

fn install_polling(
    ui: &MainWindow,
    player: Option<Rc<Player>>,
    frame_pending: Arc<AtomicBool>,
    reporter: Arc<PlaybackReporter>,
    session_ctrl: Arc<PlaybackSessionController>,
    current_item: Arc<Mutex<Option<TvItem>>>,
    cached_catalog: Arc<Mutex<Vec<TvItem>>>,
    autoplay_active: Arc<AtomicBool>,
    hwdec_active: Arc<AtomicBool>,
    headers: Vec<String>,
    credential_store: Arc<CredentialStore>,
    local_store: Arc<LocalStore>,
    jellyfin: Arc<JellyfinClient>,
) {
    let ui_weak = ui.as_weak();
    let timer = Timer::default();
    let mut tick: u64 = 0;

    let rep = reporter.clone();
    let sctrl = session_ctrl.clone();
    let cur_item = current_item.clone();
    let cat_ref = cached_catalog.clone();
    let ap_ref = autoplay_active.clone();
    let hd_ref = hwdec_active.clone();
    let hdrs = headers.clone();
    let cs = credential_store.clone();
    let ls = local_store.clone();
    let jf = jellyfin.clone();

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
                &cat_ref,
                &ap_ref,
                &hd_ref,
                &hdrs,
                &cs,
                &ls,
                &jf,
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

fn handle_event(
    ui: &MainWindow,
    player: &Player,
    event: adult_hub_player::PlayerEvent,
    reporter: &Arc<PlaybackReporter>,
    session_ctrl: &Arc<PlaybackSessionController>,
    current_item: &Arc<Mutex<Option<TvItem>>>,
    cached_catalog: &Arc<Mutex<Vec<TvItem>>>,
    autoplay_active: &Arc<AtomicBool>,
    hwdec_active: &Arc<AtomicBool>,
    headers: &[String],
    credential_store: &Arc<CredentialStore>,
    local_store: &Arc<LocalStore>,
    jellyfin: &Arc<JellyfinClient>,
) {
    use adult_hub_player::PlayerEvent;
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
            ui.set_status(format!("Loaded: {name}").into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            ui.set_playing(!player.is_paused());

            let pos_ms = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
            reporter.report_start(pos_ms, !player.is_paused());
        }
        PlayerEvent::VideoReconfig { width, height } => {
            update_texture_size(ui, width, height);
        }
        PlayerEvent::EndFile { eof, error } => {
            ui.set_playing(false);
            if let Some(error) = error {
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
                        );
                        return;
                    }
                }
                show_error(ui, format!("Playback error: {error}"));
            } else if eof {
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
                    );
                }
            }
        }
        PlayerEvent::Shutdown => {}
    }
}

fn update_texture_size(ui: &MainWindow, width: Option<u32>, height: Option<u32>) {
    if let (Some(width), Some(height)) = (width, height)
        && width > 0
        && height > 0
    {
        ui.set_texture_width(width as i32);
        ui.set_texture_height(height as i32);
    }
}

fn stats_text(stats: &PlaybackStats) -> String {
    let text = stats.to_hud_text();
    if text.is_empty() {
        "No playback statistics yet.".to_owned()
    } else {
        text
    }
}

fn show_error(ui: &MainWindow, message: impl Into<slint::SharedString>) {
    ui.set_status(message.into());
    ui.set_status_color(slint::Color::from_rgb_u8(255, 118, 118));
}

fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "0:00".to_owned();
    }
    let total = seconds.round() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn load_media(ui: &MainWindow, player: &Player, input: &str, headers: &[String]) {
    let path = resolve_media_input(input);
    ui.set_status(format!("Loading: {path}").into());
    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
    if let Err(error) = player.load_with_start(&path, headers, None) {
        show_error(ui, error.to_string());
    }
}

fn resolve_media_input(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("rtsp://")
        || trimmed.starts_with("rtmp://")
        || trimmed.starts_with("file://")
    {
        return trimmed.to_owned();
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return trimmed.to_owned();
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let candidate = directory.join(path);
        if candidate.exists() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    trimmed.to_owned()
}

#[cfg(windows)]
fn pick_media_file() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Open media")
        .add_filter(
            "Video",
            &[
                "mp4", "mkv", "webm", "mov", "avi", "ts", "m4v", "mpg", "mpeg",
            ],
        )
        .pick_file()
        .map(|path| path.to_string_lossy().into_owned())
}

#[cfg(not(windows))]
fn pick_media_file() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
            studios: vec![],
            people: vec![],
            primary_image_tag: None,
            backdrop_image_tag: None,
            thumb_image_tag: None,
            is_favorite: false,
            playback_position_ticks: 0,
            played_percentage: None,
            is_played: false,
            provider_video_id: None,
            media_source_id: None,
            container: None,
            file_size_bytes: None,
            width: None,
            height: None,
        };

        let card = tv_item_to_card(&item);
        assert_eq!(card.id, "jellyfin-123");
        assert_eq!(card.title, "Adult Scene #1");
        assert_eq!(card.year, "2025");
        assert_eq!(card.duration, "3h 20m");
        assert_eq!(card.provider, "jellyfin");
        assert!(!card.is_played);
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
}
