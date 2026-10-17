use crate::catalog::{
    filter_catalog_items, sample_catalog_tv_items, set_catalog, set_discovered_servers,
    tv_item_to_card,
};
use crate::context::AppContext;
use crate::playback::{show_error, start_item_playback};
use crate::{DiscoveredServerItem, MainWindow};
use adult_hub_client::{
    GetItemsQuery, ServiceUrlPolicy, ServiceUrlValidation, TvItem, discover_servers,
};
use adult_hub_storage::Session;
use slint::ComponentHandle;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub fn install_stage2_handlers(ui: &MainWindow, ctx: &AppContext) {
    let player = ctx.player.clone();
    let headers = ctx.headers.clone();
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let jellyfin = ctx.jellyfin.clone();
    let resolver = ctx.resolver.clone();
    let reporter = ctx.reporter.clone();
    let session_ctrl = ctx.session_ctrl.clone();
    let current_item = ctx.current_playing_item.clone();
    let cached_catalog = ctx.cached_catalog.clone();
    let phash_coord = ctx.phash_coord.clone();

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
        let phash_coord = phash_coord.clone();
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
                    let phash_coord = phash_coord.clone();
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
                                let eff_res_url = local_store
                                    .get_setting("resolver_base_url")
                                    .ok()
                                    .flatten()
                                    .or_else(|| {
                                        local_store.get_setting("resolver_url").ok().flatten()
                                    })
                                    .unwrap_or_else(|| session.server_url.clone());
                                phash_coord.update_auth(&eff_res_url, Some(&session.access_token));

                                let raw_items = match jellyfin.get_latest_items(
                                    &normalized,
                                    &session.user_id,
                                    &session.access_token,
                                    None,
                                    50,
                                ) {
                                    Ok(list) => filter_catalog_items(list, &local_store),
                                    Err(_) => filter_catalog_items(
                                        sample_catalog_tv_items(),
                                        &local_store,
                                    ),
                                };
                                let cards =
                                    raw_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
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
        let phash_coord = phash_coord.clone();
        ui.on_perform_logout(move || {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.clear_session();
            let _ = credential_store.clear();
            reporter.set_auth(None, None);
            phash_coord.update_auth("", None);
            let raw_items = filter_catalog_items(sample_catalog_tv_items(), &local_store);
            let cards = raw_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
            *cached_catalog.lock().unwrap() = raw_items;
            ui.set_is_logged_in(false);
            ui.set_logged_in_user("".into());
            set_catalog(&ui, cards);
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
        let phash_coord = phash_coord.clone();
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
            let phash_coord = phash_coord.clone();

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
                                    let eff_res_url = local_store
                                        .get_setting("resolver_base_url")
                                        .ok()
                                        .flatten()
                                        .or_else(|| {
                                            local_store.get_setting("resolver_url").ok().flatten()
                                        })
                                        .unwrap_or_else(|| session.server_url.clone());
                                    phash_coord
                                        .update_auth(&eff_res_url, Some(&session.access_token));

                                    let raw_items = match jellyfin.get_latest_items(
                                        &normalized,
                                        &session.user_id,
                                        &session.access_token,
                                        None,
                                        50,
                                    ) {
                                        Ok(list) => filter_catalog_items(list, &local_store),
                                        Err(_) => filter_catalog_items(
                                            sample_catalog_tv_items(),
                                            &local_store,
                                        ),
                                    };
                                    let cards =
                                        raw_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
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
                            ui.set_status(
                                format!("Selected server: {} ({})", info.name, info.url).into(),
                            );
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
                let raw_items = if let Some(session) = session {
                    if query.is_empty() {
                        jellyfin
                            .get_latest_items(
                                &session.server_url,
                                &session.user_id,
                                &session.access_token,
                                None,
                                50,
                            )
                            .unwrap_or_else(|_| sample_catalog_tv_items())
                    } else {
                        let q = GetItemsQuery {
                            parent_id: None,
                            start_index: 0,
                            limit: 50,
                            sort_by: "SortName",
                            sort_descending: false,
                            search_term: Some(&query),
                            ..Default::default()
                        };
                        jellyfin
                            .get_items(
                                &session.server_url,
                                &session.user_id,
                                &session.access_token,
                                &q,
                            )
                            .map(|page| page.items)
                            .unwrap_or_default()
                    }
                } else if query.is_empty() {
                    sample_catalog_tv_items()
                } else {
                    sample_catalog_tv_items()
                        .into_iter()
                        .filter(|item| item.title.to_lowercase().contains(&query.to_lowercase()))
                        .collect()
                };

                let raw_items = filter_catalog_items(raw_items, &local_store);
                let cards = raw_items.iter().map(tv_item_to_card).collect();
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
                let raw_items = match cat.as_str() {
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
                                .unwrap_or_else(|_| sample_catalog_tv_items())
                        } else {
                            sample_catalog_tv_items()
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
                            sample_catalog_tv_items()
                        } else {
                            resume_items
                        }
                    }
                    "Channels" => sample_catalog_tv_items()
                        .into_iter()
                        .filter(|i| i.is_telegram())
                        .collect(),
                    "Favorites" => {
                        if let Some(session) = &session {
                            let q = GetItemsQuery {
                                is_favorite: Some(true),
                                sort_by: "SortName",
                                limit: 50,
                                ..Default::default()
                            };
                            jellyfin
                                .get_items(
                                    &session.server_url,
                                    &session.user_id,
                                    &session.access_token,
                                    &q,
                                )
                                .map(|page| page.items)
                                .unwrap_or_else(|_| {
                                    sample_catalog_tv_items()
                                        .into_iter()
                                        .filter(|i| i.is_favorite)
                                        .collect()
                                })
                        } else {
                            sample_catalog_tv_items()
                                .into_iter()
                                .filter(|i| i.is_favorite)
                                .collect()
                        }
                    }
                    "Performers" => sample_catalog_tv_items()
                        .into_iter()
                        .filter(|i| !i.people.is_empty())
                        .collect(),
                    "Studios" => sample_catalog_tv_items()
                        .into_iter()
                        .filter(|i| !i.studios.is_empty())
                        .collect(),
                    _ => sample_catalog_tv_items(),
                };

                let raw_items = filter_catalog_items(raw_items, &local_store);
                let cards = raw_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
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
        let phash_coord = phash_coord.clone();
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
                &phash_coord,
            );
        });
    }
}
