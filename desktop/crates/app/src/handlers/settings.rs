use crate::MainWindow;
use crate::catalog::{filter_catalog_items, sample_catalog_tv_items, set_catalog, tv_item_to_card};
use crate::context::AppContext;
use crate::playback::{export_file_dialog, pick_config_file, show_error};
use slint::ComponentHandle;
use std::rc::Rc;

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
                let _ = player.stop();
                let _ = player.set_muted(true);
                let pos_ms = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
                let dur_ms = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
                if let Some(item) = current_item.lock().unwrap().as_ref() {
                    session_ctrl.checkpoint_progress(item, pos_ms, dur_ms, false);
                }
                reporter.stop_session(pos_ms);
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
            ui.set_status("Search history cleared".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 3. Clear Watch History & Progress
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
                let raw = filter_catalog_items(sample_catalog_tv_items(), &local_store);
                let cards = raw.iter().map(tv_item_to_card).collect();
                set_catalog(&ui, cards);
                *cached_catalog.lock().unwrap() = raw;
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
            let trimmed = url.trim().trim_end_matches('/').to_string();
            let _ = local_store.set_setting("resolver_base_url", &trimmed);
            let _ = local_store.set_setting("resolver_url", &trimmed);
            let session = credential_store.get().ok().flatten();
            phash_coord.update_auth(&trimmed, session.as_ref().map(|s| s.access_token.as_str()));
            ui.set_resolver_url_input(trimmed.into());
            ui.set_status("Resolver API URL updated".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 5. Test Resolver Connection
    {
        let weak = ui.as_weak();
        let resolver = resolver.clone();
        let credential_store = credential_store.clone();
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
                show_error(&ui, "No Resolver URL or server URL provided");
                return;
            }
            ui.set_status(format!("Testing Resolver connection to {url}...").into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
            let weak = weak.clone();
            let resolver = resolver.clone();
            std::thread::spawn(move || match resolver.get_categories(&url) {
                Ok(cats) => {
                    slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_status(
                                format!(
                                    "Resolver connection OK: {} categories available",
                                    cats.len()
                                )
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
                            show_error(&ui, format!("Resolver test failed: {err}"));
                        }
                    })
                    .ok();
                }
            });
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
            ui.set_seek_step_label(format!("Seek: {next}s").into());
        });
    }

    // 7. Cycle Cache Limit (128, 256, 512, 1024 MB)
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
                .unwrap_or(256);
            let next = match current {
                128 => 256,
                256 => 512,
                512 => 1024,
                _ => 128,
            };
            let _ = local_store.set_setting("cache_limit_mb", &next.to_string());
            ui.set_cache_limit_label(format!("Cache Limit: {next} MB").into());
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
        });
    }

    // 11. Unhide All Channels
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_unhide_all_channels(move || {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.clear_hidden_channels();
            let model = Rc::new(slint::VecModel::from(Vec::<slint::SharedString>::new()));
            ui.set_hidden_channels_list(model.into());
            ui.set_status("All channels unhidden".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 12. Clear Image Cache
    {
        let weak = ui.as_weak();
        ui.on_clear_image_cache(move || {
            let Some(ui) = weak.upgrade() else { return };
            let cache_dir = std::env::temp_dir().join("adult-hub-cache");
            let _ = std::fs::remove_dir_all(&cache_dir);
            ui.set_status("Image cache cleared".into());
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
        ui.on_import_config(move || {
            let Some(ui) = weak.upgrade() else { return };
            let import_path = pick_config_file();
            match import_path.and_then(|p| std::fs::read_to_string(&p).ok()) {
                Some(json) => match local_store.import_non_secret_config_json(&json) {
                    Ok(()) => {
                        crate::diagnostics::GLOBAL_LOG.i("Config", "Config imported successfully");
                        if let Ok(config) = local_store.export_non_secret_config() {
                            ui.set_seek_step_sec(config.seek_step_sec);
                            ui.set_seek_step_label(
                                format!("Seek: {}s", config.seek_step_sec).into(),
                            );
                            ui.set_cache_limit_label(
                                format!("Cache Limit: {} MB", config.cache_limit_mb).into(),
                            );
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
                        ui.set_status("Configuration imported successfully".into());
                        ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                    }
                    Err(err) => show_error(&ui, format!("Config import rejected: {err}")),
                },
                None => {
                    show_error(&ui, "No configuration file selected");
                }
            }
        });
    }

    // 15. Export Redacted Diagnostics
    {
        let weak = ui.as_weak();
        ui.on_export_diagnostics(move || {
            let Some(ui) = weak.upgrade() else { return };
            let target_path = export_file_dialog("adult-hub-diagnostics.txt", "txt");
            match crate::diagnostics::GLOBAL_LOG.export_to_file(
                &target_path,
                env!("CARGO_PKG_VERSION"),
                &[("Platform", std::env::consts::OS)],
            ) {
                Ok(()) => {
                    ui.set_status(format!("Redacted diagnostics exported to {target_path}").into());
                    ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                }
                Err(e) => show_error(&ui, format!("Diagnostics export failed: {e}")),
            }
        });
    }

    // 16. Switch Library
    {
        let weak = ui.as_weak();
        let credential_store = credential_store.clone();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        let cached_catalog = cached_catalog.clone();
        ui.on_switch_library(move || {
            let Some(ui) = weak.upgrade() else { return };
            let session = credential_store.get().ok().flatten();
            let Some(session) = session else {
                show_error(&ui, "Please log in to switch Jellyfin libraries");
                return;
            };
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let local_store = local_store.clone();
            let cached_catalog = cached_catalog.clone();
            std::thread::spawn(move || {
                match jellyfin.get_user_views(
                    &session.server_url,
                    &session.user_id,
                    &session.access_token,
                ) {
                    Ok(views) if !views.is_empty() => {
                        let current_lib_id = local_store
                            .get_setting("selected_library_id")
                            .ok()
                            .flatten();
                        let current_idx = views
                            .iter()
                            .position(|v| Some(&v.id) == current_lib_id.as_ref())
                            .unwrap_or(0);
                        let next_idx = (current_idx + 1) % views.len();
                        let next_view = &views[next_idx];
                        let _ = local_store.set_setting("selected_library_id", &next_view.id);
                        let _ = local_store.set_setting("selected_library_name", &next_view.name);

                        let raw_items = match jellyfin.get_latest_items(
                            &session.server_url,
                            &session.user_id,
                            &session.access_token,
                            Some(&next_view.id),
                            50,
                        ) {
                            Ok(list) => filter_catalog_items(list, &local_store),
                            Err(_) => filter_catalog_items(sample_catalog_tv_items(), &local_store),
                        };
                        let cards = raw_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
                        *cached_catalog.lock().unwrap() = raw_items;

                        let lib_name = next_view.name.clone();
                        slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.set_current_library(lib_name.into());
                                set_catalog(&ui, cards);
                                ui.set_status("Library switched".into());
                                ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                            }
                        })
                        .ok();
                    }
                    Ok(_) => {
                        slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.set_status("No additional libraries found".into());
                            }
                        })
                        .ok();
                    }
                    Err(e) => {
                        slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                show_error(&ui, format!("Failed to load libraries: {e}"));
                            }
                        })
                        .ok();
                    }
                }
            });
        });
    }
}
