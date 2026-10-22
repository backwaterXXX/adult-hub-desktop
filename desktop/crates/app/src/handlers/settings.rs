use crate::catalog::set_catalog;
use crate::context::AppContext;
use crate::playback::{export_file_dialog, pick_config_file, show_error};
use crate::{LibraryOptionData, MainWindow};
use adult_hub_client::{GetItemsQuery, JellyfinClient, ServiceUrlPolicy, ServiceUrlValidation};
use slint::ComponentHandle;
use std::rc::Rc;
use std::sync::atomic::Ordering;

fn refresh_content_after_filter_change(ui: &MainWindow) {
    match ui.get_last_content_screen().as_str() {
        "home" => ui.invoke_refresh_home(),
        "browse" if ui.get_search_query().trim().is_empty() => {
            ui.invoke_filter_category(ui.get_active_category());
        }
        "browse" => ui.invoke_search_catalog(ui.get_search_query()),
        "channels" => ui.invoke_select_channel_category(ui.get_active_channel_category()),
        _ => {}
    }
}

fn jellyfin_thumbnail_probe(
    jellyfin: &JellyfinClient,
    session: &adult_hub_storage::Session,
) -> String {
    let page = match jellyfin.get_items(
        &session.server_url,
        &session.user_id,
        &session.access_token,
        &GetItemsQuery {
            start_index: 0,
            limit: 8,
            sort_by: "DateCreated",
            sort_descending: true,
            include_item_types: Some("Movie"),
            ..Default::default()
        },
    ) {
        Ok(page) => page,
        Err(_) => return "Jellyfin image sample: failed to fetch movies".into(),
    };
    let cards = page
        .items
        .iter()
        .take(5)
        .map(|item| crate::catalog::tv_item_to_card_for_server(item, &session.server_url))
        .collect::<Vec<_>>();
    if cards.is_empty() {
        return "Jellyfin image sample: no movies in response".into();
    }

    let mut loaded = 0;
    let mut without_url = 0;
    for card in &cards {
        let candidates = [
            card.thumbnail_url.to_string(),
            card.fallback_thumbnail_url.to_string(),
            card.alternate_thumbnail_url.to_string(),
        ];
        let candidates = candidates
            .into_iter()
            .filter(|url| !url.is_empty())
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            without_url += 1;
            continue;
        }
        if candidates
            .iter()
            .any(|url| fetch_fresh_jellyfin_image(url, &session.access_token))
        {
            loaded += 1;
        }
    }

    if loaded == cards.len() {
        format!(
            "Jellyfin image sample: loaded {loaded}/{} posters",
            cards.len()
        )
    } else if loaded > 0 {
        format!(
            "Jellyfin image sample: partial failure, loaded {loaded}/{} posters; {without_url} without URL",
            cards.len()
        )
    } else {
        format!(
            "Jellyfin image sample: failed for 0/{} posters; {without_url} without URL (check Export Diagnostics)",
            cards.len()
        )
    }
}

fn fetch_fresh_jellyfin_image(url: &str, token: &str) -> bool {
    if crate::thumbnails::fetch_fresh_image(url, Some(token)).is_some() {
        return true;
    }
    let Ok(mut untagged) = url::Url::parse(url) else {
        return false;
    };
    let had_tag = untagged.query_pairs().any(|(key, _)| key == "tag");
    if !had_tag {
        return false;
    }
    let params = untagged
        .query_pairs()
        .filter(|(key, _)| key != "tag")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    untagged.set_query(None);
    if !params.is_empty() {
        untagged.query_pairs_mut().extend_pairs(params);
    }
    crate::thumbnails::fetch_fresh_image(untagged.as_str(), Some(token)).is_some()
}

fn resolver_thumbnail_probe(
    resolver: &adult_hub_client::ResolverClient,
    base_url: &str,
) -> (String, String) {
    let categories = match resolver.get_categories(base_url) {
        Ok(categories) => categories,
        Err(error) => {
            return (
                format!("Unavailable: {error}"),
                "Resolver image sample: failed to fetch categories".into(),
            );
        }
    };
    let connection_status = format!("Connected (categories: {})", categories.len());
    let Some(category) = categories.first() else {
        return (
            connection_status,
            "Resolver image sample: no categories available".into(),
        );
    };
    let videos = match resolver.get_category_videos(base_url, &category.id, 0, "latest", None) {
        Ok(videos) => videos,
        Err(_) => {
            return (
                connection_status,
                "Resolver image sample: failed to fetch channel item".into(),
            );
        }
    };
    let Some(image_url) = videos
        .iter()
        .filter_map(|video| video.thumbnail.as_deref())
        .find_map(crate::thumbnails::resolve_resolver_thumbnail_url)
    else {
        return (
            connection_status,
            "Resolver image sample: no thumbnail in sample".into(),
        );
    };
    if crate::thumbnails::fetch_fresh_image(&image_url, None).is_some() {
        (connection_status, "Resolver image sample: OK".into())
    } else {
        (
            connection_status,
            "Resolver image sample: error (check Export Diagnostics)".into(),
        )
    }
}

pub fn install_stage4_handlers(ui: &MainWindow, ctx: &AppContext) {
    let player = ctx.player.clone();
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let jellyfin = ctx.jellyfin.clone();
    let resolver = ctx.resolver.clone();
    let phash_coord = ctx.phash_coord.clone();
    let reporter = ctx.reporter.clone();
    let session_ctrl = ctx.session_ctrl.clone();
    let current_item = ctx.current_playing_item.clone();
    let cached_catalog = ctx.cached_catalog.clone();
    let autoplay_active = ctx.autoplay_active.clone();

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_refresh_system_info(move || {
            if let Some(ui) = weak.upgrade() {
                crate::system_info::update_ui(&ui, player.as_deref());
                ui.set_status("System information refreshed".into());
                ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_confirm_dialog_accepted(move |action| {
            let Some(ui) = weak.upgrade() else { return };
            match action.as_str() {
                "search" => ui.invoke_clear_search_history(),
                "watch" => ui.invoke_clear_watch_history(),
                "progress" => ui.invoke_clear_progress_history(),
                "cache" => ui.invoke_clear_image_cache(),
                _ => {}
            }
        });
    }

    // 1. Emergency Panic Exit (Immediately hide, mute/stop, checkpoint, exit)
    {
        let weak = ui.as_weak();
        let player = player.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let reporter = reporter.clone();
        ui.on_panic_exit(move || {
            crate::diagnostics::GLOBAL_LOG.w(
                "Privacy",
                "Panic exit triggered; terminating process immediately",
            );
            if let Some(ui) = weak.upgrade() {
                let _ = ui.window().hide();
            }
            if let Some(player) = &player {
                let pos_ms = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
                let dur_ms = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
                let _ = player.set_muted(true);
                if let Some(item) = current_item.lock().unwrap().as_ref() {
                    session_ctrl.checkpoint_progress(item, pos_ms, dur_ms, false);
                }
                reporter.stop_session(pos_ms);
                let _ = player.stop();
            }
            std::process::exit(0);
        });
    }

    // 2. Clear Search History
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_clear_search_history(move || {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.clear_search_history();
            ui.set_recent_searches(
                Rc::new(slint::VecModel::from(Vec::<slint::SharedString>::new())).into(),
            );
            ui.set_status("Search history cleared".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 3. Clear Resume Progress
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_clear_progress_history(move || {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.clear_all_progress();
            ui.set_status("Resume progress cleared".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            // Telegram resume data is local. Refresh the previous screen so
            // server-backed Jellyfin resume items remain visible and local
            // Telegram positions disappear from shelves and channel cards.
            refresh_content_after_filter_change(&ui);
        });
    }

    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let cached_catalog = cached_catalog.clone();
        ui.on_clear_watch_history(move || {
            let Some(ui) = weak.upgrade() else { return };
            if let Err(error) = local_store.clear_played_history() {
                ui.set_status(format!("Could not clear watch history: {error}").into());
                ui.set_status_color(slint::Color::from_rgb_u8(244, 67, 54));
                return;
            }
            ui.set_status("Watch history cleared".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            if ui.get_last_content_screen() == "home" {
                ui.invoke_refresh_home();
            } else if ui.get_active_category() == "Watch History" {
                set_catalog(&ui, Vec::new());
                cached_catalog.lock().unwrap().clear();
            }
        });
    }

    // 4. Save Resolver Base URL
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let phash_coord = phash_coord.clone();
        ui.on_save_resolver_url(move |url| {
            let Some(ui) = weak.upgrade() else { return };
            let mut trimmed = url.trim().trim_end_matches('/').to_string();
            if !trimmed.is_empty() {
                match ServiceUrlPolicy::inspect(&trimmed, false) {
                    ServiceUrlValidation::Valid(service_url) => {
                        if !super::request_cleartext_consent(
                            &ui,
                            &local_store,
                            &service_url.normalized,
                            "resolver-save",
                        ) {
                            return;
                        }
                        trimmed = service_url.normalized;
                    }
                    ServiceUrlValidation::Invalid(error) => {
                        show_error(&ui, error);
                        return;
                    }
                }
            }
            let _ = local_store.set_setting("resolver_base_url", &trimmed);
            let _ = local_store.set_setting("resolver_url", &trimmed);
            crate::thumbnails::set_resolver_base_url(&trimmed);
            let session = credential_store.get().ok().flatten();
            crate::thumbnails::set_resolver_auth_token(
                session
                    .as_ref()
                    .map(|session| session.access_token.as_str()),
            );
            phash_coord.update_auth(&trimmed, session.as_ref().map(|s| s.access_token.as_str()));
            ui.set_resolver_url_input(trimmed.clone().into());
            ui.set_saved_resolver_url(trimmed.into());
            ui.set_resolver_url_dirty(false);
            ui.set_status("Zaktualizowano adres serwera wideo".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            ui.invoke_test_resolver_connection();
        });
    }

    // 5. Test Resolver Connection
    {
        let weak = ui.as_weak();
        let resolver = resolver.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        ui.on_test_resolver_connection(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut url = ui
                .get_resolver_url_input()
                .trim()
                .trim_end_matches('/')
                .to_string();
            if url.is_empty() {
                if let Some(session) = credential_store.get().ok().flatten() {
                    url = session.server_url.trim_end_matches('/').to_string();
                }
            }
            if url.is_empty() {
                show_error(&ui, "Nie podano adresu serwera wideo ani resolvera");
                return;
            }
            match ServiceUrlPolicy::inspect(&url, false) {
                ServiceUrlValidation::Valid(service_url) => {
                    if !super::request_cleartext_consent(
                        &ui,
                        &local_store,
                        &service_url.normalized,
                        "resolver-test",
                    ) {
                        return;
                    }
                    url = service_url.normalized;
                }
                ServiceUrlValidation::Invalid(error) => {
                    show_error(&ui, error);
                    return;
                }
            }
            ui.set_status(format!("Testing video server connection {url}…").into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
            let weak = weak.clone();
            let resolver = resolver.clone();
            std::thread::spawn(move || match resolver.get_categories(&url) {
                Ok(cats) => {
                    slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_status(
                                format!("Connection OK: {} categories available", cats.len())
                                    .into(),
                            );
                            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                        }
                    })
                    .ok();
                }
                Err(err) => {
                    slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            show_error(&ui, format!("Connection test failed: {err}"));
                        }
                    })
                    .ok();
                }
            });
        });
    }

    // Check both configured services and surface their results in Settings.
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let resolver = resolver.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        ui.on_check_connections(move || {
            let Some(ui) = weak.upgrade() else { return };
            let session = credential_store.get().ok().flatten();
            if let Some(session) = session.as_ref()
                && !super::request_cleartext_consent(
                    &ui,
                    &local_store,
                    &session.server_url,
                    "check-connections",
                )
            {
                return;
            }

            let mut resolver_url = ui
                .get_resolver_url_input()
                .trim()
                .trim_end_matches('/')
                .to_string();
            if resolver_url.is_empty() {
                resolver_url = session
                    .as_ref()
                    .map(|session| session.server_url.clone())
                    .unwrap_or_default();
            }
            if !resolver_url.is_empty() {
                match ServiceUrlPolicy::inspect(&resolver_url, false) {
                    ServiceUrlValidation::Valid(service_url) => {
                        if !super::request_cleartext_consent(
                            &ui,
                            &local_store,
                            &service_url.normalized,
                            "check-connections",
                        ) {
                            return;
                        }
                        resolver_url = service_url.normalized;
                    }
                    ServiceUrlValidation::Invalid(error) => {
                        show_error(&ui, error);
                        return;
                    }
                }
            }

            ui.set_checking_connections(true);
            ui.set_jellyfin_connection_status(if session.is_some() {
                "Checking…".into()
            } else {
                "Not connected".into()
            });
            ui.set_resolver_connection_status(if resolver_url.is_empty() {
                "Not configured".into()
            } else {
                "Checking…".into()
            });
            ui.set_thumbnail_check_status("Checking artwork samples…".into());
            ui.set_thumbnail_check_failed(false);

            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let resolver = resolver.clone();
            std::thread::spawn(move || {
                let (
                    jellyfin_status,
                    jellyfin_thumbnail_status,
                    resolver_status,
                    resolver_thumbnail_status,
                ) = std::thread::scope(|scope| {
                    let jellyfin_worker = scope.spawn(|| {
                        let connection_status = match session.as_ref() {
                            Some(session) => match jellyfin.get_user_views(
                                &session.server_url,
                                &session.user_id,
                                &session.access_token,
                            ) {
                                Ok(libraries) => {
                                    format!("Connected ({} libraries)", libraries.len())
                                }
                                Err(error) => format!("Unavailable: {error}"),
                            },
                            None => "Not connected".to_string(),
                        };
                        let thumbnail_status = session.as_ref().map_or_else(
                            || "Jellyfin: not connected".to_string(),
                            |session| jellyfin_thumbnail_probe(&jellyfin, session),
                        );
                        (connection_status, thumbnail_status)
                    });
                    let resolver_worker = scope.spawn(|| {
                        if resolver_url.is_empty() {
                            (
                                "Not configured".to_string(),
                                "Resolver: not configured".to_string(),
                            )
                        } else {
                            resolver_thumbnail_probe(&resolver, &resolver_url)
                        }
                    });
                    let jellyfin_result = jellyfin_worker.join().unwrap_or_else(|_| {
                        (
                            "Unavailable: diagnostic worker error".to_string(),
                            "Jellyfin image sample: error".to_string(),
                        )
                    });
                    let resolver_result = resolver_worker.join().unwrap_or_else(|_| {
                        (
                            "Unavailable: diagnostic worker error".to_string(),
                            "Resolver image sample: error".to_string(),
                        )
                    });
                    (
                        jellyfin_result.0,
                        jellyfin_result.1,
                        resolver_result.0,
                        resolver_result.1,
                    )
                });
                let thumbnail_status =
                    format!("{jellyfin_thumbnail_status} · {resolver_thumbnail_status}");
                let thumbnail_failed = thumbnail_status.contains("failed")
                    || thumbnail_status.contains("error")
                    || thumbnail_status.contains("failure")
                    || thumbnail_status.contains("unavailable")
                    || thumbnail_status.contains("couldn't")
                    || thumbnail_status.contains("no images")
                    || thumbnail_status.contains("no movies")
                    || thumbnail_status.contains("no thumbnail")
                    || thumbnail_status.contains("no categories")
                    || thumbnail_status.contains("not connected")
                    || thumbnail_status.contains("not configured");

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_jellyfin_connection_status(jellyfin_status.into());
                        ui.set_resolver_connection_status(resolver_status.into());
                        ui.set_thumbnail_check_status(thumbnail_status.into());
                        ui.set_thumbnail_check_failed(thumbnail_failed);
                        ui.set_checking_connections(false);
                    }
                });
            });
        });
    }

    // Remove a saved HTTP trust grant and stop using its configured services.
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let phash_coord = phash_coord.clone();
        ui.on_revoke_cleartext_http_origin(move |origin| {
            let Some(ui) = weak.upgrade() else { return };
            let origin = origin.to_string();
            if let Err(error) = local_store.revoke_cleartext_origin(&origin) {
                show_error(&ui, format!("Could not revoke HTTP consent: {error}"));
                return;
            }

            let session = credential_store.get().ok().flatten();
            let active_server_uses_origin = session.as_ref().is_some_and(|session| {
                matches!(
                    ServiceUrlPolicy::inspect(&session.server_url, false),
                    ServiceUrlValidation::Valid(ref url) if url.origin == origin
                )
            });
            let resolver_url = ui.get_resolver_url_input().to_string();
            let active_resolver_uses_origin = matches!(
                ServiceUrlPolicy::inspect(&resolver_url, false),
                ServiceUrlValidation::Valid(url) if url.origin == origin
            );

            if active_resolver_uses_origin {
                let _ = local_store.set_setting("resolver_base_url", "");
                let _ = local_store.set_setting("resolver_url", "");
                crate::thumbnails::set_resolver_base_url("");
                crate::thumbnails::set_resolver_auth_token(
                    session
                        .as_ref()
                        .map(|session| session.access_token.as_str()),
                );
                ui.set_resolver_url_input("".into());
                if !active_server_uses_origin {
                    let session_url = session
                        .as_ref()
                        .map(|session| session.server_url.as_str())
                        .unwrap_or_default();
                    crate::thumbnails::set_resolver_base_url(session_url);
                    crate::thumbnails::set_resolver_auth_token(
                        session
                            .as_ref()
                            .map(|session| session.access_token.as_str()),
                    );
                    phash_coord.update_auth(
                        session_url,
                        session
                            .as_ref()
                            .map(|session| session.access_token.as_str()),
                    );
                }
            }
            if active_server_uses_origin {
                ui.invoke_perform_logout();
            }

            super::refresh_approved_http_origins(&ui, &local_store);
            ui.set_status(format!("HTTP trust revoked for {origin}").into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 6. Cycle Seek Step (10s, 15s, 30s)
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_cycle_seek_step(move || {
            let Some(ui) = weak.upgrade() else { return };
            let current = ui.get_seek_step_sec();
            let next = match current {
                10 => 15,
                15 => 30,
                _ => 10,
            };
            let _ = local_store.set_setting("seek_step_sec", &next.to_string());
            ui.set_seek_step_sec(next);
            ui.set_seek_step_label(format!("Step: {next}s").into());
        });
    }

    // 7. Cycle the persistent thumbnail cache size
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_cycle_cache_limit(move || {
            let Some(ui) = weak.upgrade() else { return };
            let current = local_store
                .get_setting("cache_limit_mb")
                .ok()
                .flatten()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(1024);
            let next = match current {
                0 => 1024,
                1024 => 2048,
                2048 => 5120,
                5120 => 10240,
                10240 => 20480,
                20480 => 0,
                _ => 1024,
            };
            let _ = local_store.set_setting("cache_limit_mb", &next.to_string());
            crate::thumbnails::set_cache_limit_mb_async(next as usize);
            let limit_str = if next == 0 {
                "Off (RAM only)".to_string()
            } else if next >= 1024 {
                format!("{} GB", next / 1024)
            } else {
                format!("{next} MB")
            };
            ui.set_cache_limit_label(format!("Cache Limit: {limit_str}").into());
            ui.set_cache_limit_mb(next);
            let ui_weak = ui.as_weak();
            std::thread::spawn(move || {
                let used_bytes = crate::thumbnails::current_cache_size_bytes();
                let used_str = crate::thumbnails::format_cache_bytes(used_bytes);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_cache_usage_text(
                            format!("Used: {used_str} / Limit: {limit_str}").into(),
                        );
                    }
                });
            });
        });
    }

    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_set_cache_limit_mb(move |mb| {
            let Some(ui) = weak.upgrade() else { return };
            let mb = mb.max(0);
            let _ = local_store.set_setting("cache_limit_mb", &mb.to_string());
            crate::thumbnails::set_cache_limit_mb_async(mb as usize);
            let limit_str = if mb == 0 {
                "Off (RAM only)".to_string()
            } else if mb >= 1024 {
                format!("{} GB", mb / 1024)
            } else {
                format!("{mb} MB")
            };
            ui.set_cache_limit_label(format!("Cache Limit: {limit_str}").into());
            ui.set_cache_limit_mb(mb);
            let ui_weak = ui.as_weak();
            std::thread::spawn(move || {
                let used_bytes = crate::thumbnails::current_cache_size_bytes();
                let used_str = crate::thumbnails::format_cache_bytes(used_bytes);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_cache_usage_text(
                            format!("Used: {used_str} / Limit: {limit_str}").into(),
                        );
                    }
                });
            });
        });
    }

    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_refresh_cache_size(move || {
            let Some(ui) = weak.upgrade() else { return };
            let current = local_store
                .get_setting("cache_limit_mb")
                .ok()
                .flatten()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(1024);
            let limit_str = if current == 0 {
                "Off (RAM only)".to_string()
            } else if current >= 1024 {
                format!("{} GB", current / 1024)
            } else {
                format!("{current} MB")
            };
            let ui_weak = ui.as_weak();
            std::thread::spawn(move || {
                let used_bytes = crate::thumbnails::current_cache_size_bytes();
                let used_str = crate::thumbnails::format_cache_bytes(used_bytes);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_cache_usage_text(
                            format!("Used: {used_str} / Limit: {limit_str}").into(),
                        );
                    }
                });
            });
        });
    }

    // 8. Toggle Low-RAM Mode
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_toggle_low_ram(move || {
            let Some(ui) = weak.upgrade() else { return };
            let active = !ui.get_low_ram_active();
            let _ = local_store.set_setting("low_ram_mode", if active { "true" } else { "false" });
            crate::thumbnails::set_low_ram_mode(active);
            ui.set_low_ram_active(active);
            ui.set_status(
                if active {
                    "Low-RAM mode enabled"
                } else {
                    "Low-RAM mode disabled"
                }
                .into(),
            );
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            // Settings is the active screen while this toggle is clicked. Refresh the
            // content screen the user came from so regenerated URLs use the new size.
            refresh_content_after_filter_change(&ui);
        });
    }

    // 9. Toggle Video Matching (pHash)
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_toggle_phash(move || {
            let Some(ui) = weak.upgrade() else { return };
            let active = !ui.get_phash_active();
            let _ =
                local_store.set_setting("phash_on_playback", if active { "true" } else { "false" });
            ui.set_phash_active(active);
            ui.set_status(
                if active {
                    "Video matching (pHash) enabled"
                } else {
                    "Video matching disabled"
                }
                .into(),
            );
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 10. Excluded Tags
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_add_excluded_tag(move |tag| {
            let Some(ui) = weak.upgrade() else { return };
            let tag = tag.trim().to_string();
            if !tag.is_empty() {
                let _ = local_store.exclude_tag(&tag);
                if let Ok(tags) = local_store.get_excluded_tags() {
                    let model = Rc::new(slint::VecModel::from(
                        tags.into_iter()
                            .map(slint::SharedString::from)
                            .collect::<Vec<_>>(),
                    ));
                    ui.set_excluded_tags_list(model.into());
                }
                ui.set_new_tag_input("".into());
                refresh_content_after_filter_change(&ui);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_remove_excluded_tag(move |tag| {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.remove_excluded_tag(&tag);
            if let Ok(tags) = local_store.get_excluded_tags() {
                let model = Rc::new(slint::VecModel::from(
                    tags.into_iter()
                        .map(slint::SharedString::from)
                        .collect::<Vec<_>>(),
                ));
                ui.set_excluded_tags_list(model.into());
            }
            refresh_content_after_filter_change(&ui);
        });
    }

    // 11. Unhide channels
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_unhide_channel(move |channel_id| {
            let Some(ui) = weak.upgrade() else { return };
            if let Err(error) = local_store.unhide_channel(&channel_id) {
                ui.set_status(format!("Could not unhide channel: {error}").into());
                ui.set_status_color(slint::Color::from_rgb_u8(244, 67, 54));
                return;
            }
            let channels = local_store.get_hidden_channels().unwrap_or_default();
            ui.set_hidden_channels_list(
                Rc::new(slint::VecModel::from(
                    channels
                        .into_iter()
                        .map(slint::SharedString::from)
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
            ui.invoke_refresh_channel_categories();
            ui.set_status("Channel unhidden".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 12. Unhide All Channels
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_unhide_all_channels(move || {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.clear_hidden_channels();
            let model = Rc::new(slint::VecModel::from(Vec::<slint::SharedString>::new()));
            ui.set_hidden_channels_list(model.into());
            ui.invoke_refresh_channel_categories();
            ui.set_status("All channels unhidden".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 12. Clear Image Cache
    {
        let weak = ui.as_weak();
        ui.on_clear_image_cache(move || {
            let Some(ui) = weak.upgrade() else { return };
            crate::thumbnails::clear_cache();
            match ui.get_last_content_screen().as_str() {
                "browse" => {
                    if ui.get_is_catalog_search_results() {
                        ui.invoke_search_catalog(ui.get_search_applied_query());
                    } else {
                        ui.invoke_filter_category(ui.get_active_category());
                    }
                }
                "channels" => ui.invoke_select_channel_category(ui.get_active_channel_category()),
                _ => ui.invoke_refresh_home(),
            }
            ui.set_status("Image cache cleared; reloading artwork…".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 13. Export Configuration
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_export_config(move || {
            let Some(ui) = weak.upgrade() else { return };
            match local_store.export_non_secret_config_json() {
                Ok(json) => {
                    let target_path = export_file_dialog("adult-hub-config.json", "json");
                    match std::fs::write(&target_path, json) {
                        Ok(_) => {
                            crate::diagnostics::GLOBAL_LOG
                                .i("Config", &format!("Config exported to {target_path}"));
                            ui.set_status(format!("Config exported to {target_path}").into());
                            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                        }
                        Err(e) => show_error(&ui, format!("Failed to write config: {e}")),
                    }
                }
                Err(e) => show_error(&ui, format!("Export error: {e}")),
            }
        });
    }

    // 14. Import Configuration
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let phash_coord = phash_coord.clone();
        let credential_store = credential_store.clone();
        let autoplay_active = autoplay_active.clone();
        ui.on_import_config(move || {
            let Some(ui) = weak.upgrade() else { return };
            let import_path = pick_config_file();
            match import_path.and_then(|p| std::fs::read_to_string(&p).ok()) {
                Some(json) => match local_store.import_non_secret_config_json(&json) {
                    Ok(()) => {
                        crate::diagnostics::GLOBAL_LOG.i("Config", "Config imported successfully");
                        if let Ok(config) = local_store.export_non_secret_config() {
                            crate::thumbnails::set_cache_limit_mb(
                                config.cache_limit_mb.max(0) as usize
                            );
                            ui.set_seek_step_sec(config.seek_step_sec);
                            ui.set_seek_step_label(
                                format!("Step: {}s", config.seek_step_sec).into(),
                            );
                            ui.set_cache_limit_label(if config.cache_limit_mb == 0 {
                                "Off (RAM only)".into()
                            } else {
                                format!("Cache Limit: {} MB", config.cache_limit_mb).into()
                            });
                            ui.set_phash_active(config.phash_on_playback);
                            autoplay_active.store(config.autoplay_next, Ordering::SeqCst);
                            ui.set_autoplay_active(config.autoplay_next);
                            crate::thumbnails::set_low_ram_mode(config.low_ram_mode);
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
                        ui.set_status("Configuration imported successfully".into());
                        ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                        let imported_resolver_url = local_store
                            .get_setting("resolver_base_url")
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        if !imported_resolver_url.is_empty() {
                            match ServiceUrlPolicy::inspect(&imported_resolver_url, false) {
                                ServiceUrlValidation::Valid(service_url)
                                    if service_url.uses_cleartext
                                        && !super::request_cleartext_consent(
                                            &ui,
                                            &local_store,
                                            &service_url.normalized,
                                            "resolver-save",
                                        ) =>
                                {
                                    let _ = local_store.set_setting("resolver_base_url", "");
                                    let _ = local_store.set_setting("resolver_url", "");
                                    ui.set_resolver_url_input(service_url.normalized.into());
                                    let session = credential_store.get().ok().flatten();
                                    phash_coord.update_auth(
                                        "",
                                        session
                                            .as_ref()
                                            .map(|session| session.access_token.as_str()),
                                    );
                                }
                                ServiceUrlValidation::Invalid(error) => {
                                    let _ = local_store.set_setting("resolver_base_url", "");
                                    let _ = local_store.set_setting("resolver_url", "");
                                    ui.set_resolver_url_input("".into());
                                    show_error(
                                        &ui,
                                        format!("Importowany adres serwera odrzucony: {error}"),
                                    );
                                }
                                _ => {}
                            }
                        }
                        let active_resolver_url = local_store
                            .get_setting("resolver_base_url")
                            .ok()
                            .flatten()
                            .filter(|url| !url.is_empty())
                            .or_else(|| {
                                credential_store
                                    .get()
                                    .ok()
                                    .flatten()
                                    .map(|session| session.server_url)
                            })
                            .unwrap_or_default();
                        crate::thumbnails::set_resolver_base_url(&active_resolver_url);
                        let active_token = credential_store
                            .get()
                            .ok()
                            .flatten()
                            .map(|session| session.access_token);
                        crate::thumbnails::set_resolver_auth_token(active_token.as_deref());
                    }
                    Err(err) => show_error(&ui, format!("Odrzucono import konfiguracji: {err}")),
                },
                None => {
                    show_error(&ui, "Nie wybrano pliku konfiguracji");
                }
            }
        });
    }

    // 15. Export Redacted Diagnostics
    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_export_diagnostics(move || {
            let Some(ui) = weak.upgrade() else { return };
            let target_path = export_file_dialog("adult-hub-diagnostics.txt", "txt");
            let info = crate::system_info::snapshot();
            let os = format!("{} {}", info.os_name, info.os_version);
            let processors = info.logical_processors.to_string();
            let total_memory = format!("{} MB", info.total_memory_mb);
            let available_memory = format!("{} MB", info.available_memory_mb);
            let vulkan_loader = info.vulkan_loader.clone();
            let decoder_support = match player
                .as_deref()
                .and_then(adult_hub_player::Player::common_video_decoder_support)
            {
                Some(support) => format!(
                    "H.264={}, HEVC={}, VP9={}, AV1={}",
                    support[0], support[1], support[2], support[3]
                ),
                None => "Unavailable".to_owned(),
            };
            let extra = [
                ("OS", os.as_str()),
                ("Processor", info.cpu_brand.as_str()),
                ("Logical processors", processors.as_str()),
                ("Total RAM", total_memory.as_str()),
                ("Available RAM", available_memory.as_str()),
                ("Vulkan loader", vulkan_loader.as_str()),
                ("libmpv decoder support", decoder_support.as_str()),
            ];
            match crate::diagnostics::GLOBAL_LOG.export_to_file(
                &target_path,
                env!("CARGO_PKG_VERSION"),
                &extra,
            ) {
                Ok(()) => {
                    ui.set_status(format!("Redacted diagnostics exported to {target_path}").into());
                    ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                }
                Err(e) => show_error(&ui, format!("Diagnostics export failed: {e}")),
            }
        });
    }

    // 16. List and select Jellyfin libraries.
    {
        let weak = ui.as_weak();
        let credential_store = credential_store.clone();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        ui.on_refresh_library_list(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(session) = credential_store.get().ok().flatten() else {
                ui.set_status("Sign in to load Jellyfin libraries".into());
                ui.set_status_color(slint::Color::from_rgb_u8(144, 152, 168));
                return;
            };
            ui.set_status("Loading Jellyfin libraries…".into());
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let local_store = local_store.clone();
            std::thread::spawn(move || {
                let selected_id = local_store
                    .get_setting("selected_library_id")
                    .ok()
                    .flatten();
                let result = jellyfin.get_user_views(
                    &session.server_url,
                    &session.user_id,
                    &session.access_token,
                );
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    match result {
                        Ok(views) => {
                            let options = views
                                .into_iter()
                                .map(|view| LibraryOptionData {
                                    selected: selected_id.as_deref() == Some(view.id.as_str()),
                                    id: view.id.into(),
                                    name: view.name.into(),
                                })
                                .collect::<Vec<_>>();
                            ui.set_library_options(Rc::new(slint::VecModel::from(options)).into());
                        }
                        Err(error) => show_error(&ui, format!("Could not load libraries: {error}")),
                    }
                })
                .ok();
            });
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_switch_library(move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_refresh_library_list();
            }
        });
    }
    {
        let weak = ui.as_weak();
        let credential_store = credential_store.clone();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        ui.on_select_library(move |library_id| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(session) = credential_store.get().ok().flatten() else {
                show_error(&ui, "Please log in to select a Jellyfin library");
                return;
            };
            let library_id = library_id.to_string();
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let local_store = local_store.clone();
            std::thread::spawn(move || {
                let result = jellyfin
                    .get_user_views(&session.server_url, &session.user_id, &session.access_token)
                    .map(|views| views.into_iter().find(|view| view.id == library_id));
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    match result {
                        Ok(Some(view)) => {
                            if let Err(error) = local_store
                                .set_setting("selected_library_id", &view.id)
                                .and_then(|_| {
                                    local_store.set_setting("selected_library_name", &view.name)
                                })
                            {
                                show_error(
                                    &ui,
                                    format!("Could not save selected library: {error}"),
                                );
                                return;
                            }
                            ui.set_current_library(view.name.into());
                            ui.set_active_category("Latest".into());
                            ui.set_search_query("".into());
                            ui.invoke_reset_catalog_text_filters();
                            ui.set_status("Active library changed".into());
                            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                            ui.invoke_refresh_library_list();
                            ui.invoke_refresh_home_categories();
                            ui.invoke_refresh_home();
                            if ui.get_active_screen() == "browse" {
                                ui.invoke_filter_category("Latest".into());
                            }
                        }
                        Ok(None) => show_error(&ui, "The selected library is no longer available"),
                        Err(error) => show_error(&ui, format!("Could not select library: {error}")),
                    }
                })
                .ok();
            });
        });
    }
}
