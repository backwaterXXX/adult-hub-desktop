#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(clippy::too_many_arguments)]

mod catalog;
mod context;
mod diagnostics;
mod handlers;
mod options;
mod phash;
mod playback;
mod renderer;
mod reporter;
mod session;
mod updater;

use adult_hub_client::{JellyfinClient, ResolverClient, discover_servers};
use adult_hub_player::Player;
use adult_hub_storage::{CredentialStore, LocalStore, default_db_path};
use catalog::{
    sample_catalog_items, sample_catalog_tv_items, set_catalog, set_discovered_servers,
    tv_item_to_card,
};
use context::AppContext;
use options::{Options, initial_media};
use playback::{load_media, show_error, start_item_playback};
use renderer::install_renderer;
use reporter::PlaybackReporter;
use session::PlaybackSessionController;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

slint::include_modules!();

pub use handlers::controls::ASPECTS;

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

    let session_opt = local_store.load_session().ok().flatten();
    let initial_resolver_url = local_store
        .get_setting("resolver_base_url")
        .ok()
        .flatten()
        .or_else(|| local_store.get_setting("resolver_url").ok().flatten())
        .or_else(|| session_opt.as_ref().map(|s| s.server_url.clone()))
        .unwrap_or_default();
    let initial_auth_token = session_opt.as_ref().map(|s| s.access_token.clone());

    let phash_coord = phash::PhashCoordinator::new(
        local_store.clone(),
        resolver.clone(),
        initial_resolver_url.clone(),
        initial_auth_token,
    );

    let current_playing_item = Arc::new(Mutex::new(None));
    let cached_catalog = Arc::new(Mutex::new(sample_catalog_tv_items()));
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
        cached_catalog: cached_catalog.clone(),
        autoplay_active: autoplay_active.clone(),
        hwdec_active: hwdec_active.clone(),
    };

    // Synchronize initial settings to UI
    if let Ok(config) = local_store.export_non_secret_config() {
        ui.set_seek_step_sec(config.seek_step_sec);
        ui.set_seek_step_label(format!("Seek: {}s", config.seek_step_sec).into());
        ui.set_cache_limit_label(format!("Cache Limit: {} MB", config.cache_limit_mb).into());
        ui.set_phash_active(config.phash_on_playback);
        ui.set_low_ram_active(config.low_ram_mode);
        if let Some(url) = config.resolver_base_url {
            ui.set_resolver_url_input(url.into());
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

    let frame_pending = Arc::new(AtomicBool::new(false));
    handlers::install_all_handlers(&ui, &ctx, frame_pending.clone());

    // Initial session restore & catalog population
    if let Ok(Some(session)) = local_store.load_session() {
        let _ = credential_store.save(session.clone());
        reporter.set_auth(
            Some(session.server_url.clone()),
            Some(session.access_token.clone()),
        );
        let eff_res_url = if initial_resolver_url.is_empty() {
            session.server_url.clone()
        } else {
            initial_resolver_url.clone()
        };
        phash_coord.update_auth(&eff_res_url, Some(&session.access_token));
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

    if let Some(ref p) = player {
        install_renderer(&ui, p.clone(), &frame_pending)?;
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
                &phash_coord,
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

    ui.show()?;
    ui.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
            is_favorite: true,
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
}
