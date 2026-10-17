use crate::catalog::{filter_catalog_items, sample_catalog_tv_items, set_catalog, tv_item_to_card};
use crate::context::AppContext;
use crate::playback::{format_time, show_error, start_item_playback_with_resume};
use crate::session::PlaybackSessionController;
use crate::{MainWindow, VideoCardData};
use adult_hub_client::{GetItemsQuery, TvItem};
use slint::ComponentHandle;
use std::path::Path;
use std::rc::Rc;

pub fn install_stage6_handlers(ui: &MainWindow, ctx: &AppContext) {
    let player = ctx.player.clone();
    let headers = ctx.headers.clone();
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let jellyfin = ctx.jellyfin.clone();
    let reporter = ctx.reporter.clone();
    let session_ctrl = ctx.session_ctrl.clone();
    let current_item = ctx.current_playing_item.clone();
    let cached_catalog = ctx.cached_catalog.clone();
    let phash_coord = ctx.phash_coord.clone();

    // 1. Open Item Detail Modal
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let local_store = local_store.clone();
        let session_ctrl = session_ctrl.clone();
        ui.on_open_item_detail(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let id = id.to_string();
            let target_item = cached_catalog
                .lock()
                .unwrap()
                .iter()
                .find(|i| i.id == id)
                .cloned()
                .unwrap_or_else(|| {
                    if id == "sample" {
                        sample_catalog_tv_items()[0].clone()
                    } else {
                        TvItem {
                            id: id.clone(),
                            title: id.clone(),
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

            let year_str = target_item.year.map(|y| y.to_string()).unwrap_or_default();
            let duration_str = target_item.formatted_duration();
            let resolution_str = match (target_item.width, target_item.height) {
                (Some(w), Some(h)) => format!("{w}x{h}"),
                _ => String::new(),
            };
            let studio_str = target_item.studios.first().cloned().unwrap_or_default();
            let genres_str = target_item.genres.join(", ");
            let overview_str = target_item.overview.clone().unwrap_or_default();

            // Calculate resume information
            let duration_ms = target_item
                .runtime_ticks
                .map(|t| (t / 10_000) as u64)
                .unwrap_or(0);
            let resume_ms = if target_item.is_telegram() {
                let prov_id = target_item
                    .provider_video_id
                    .as_deref()
                    .or_else(|| target_item.id.strip_prefix("telegram:"))
                    .unwrap_or(&target_item.id);
                session_ctrl.get_telegram_resume_position(prov_id)
            } else if target_item.playback_position_ticks > 0 {
                Some((target_item.playback_position_ticks / 10_000) as u64)
            } else {
                local_store
                    .get_progress("local", &target_item.id)
                    .ok()
                    .flatten()
                    .and_then(|(pos, _)| if pos > 0 { Some(pos) } else { None })
            };

            let can_resume = resume_ms
                .is_some_and(|ms| PlaybackSessionController::should_prompt_resume(ms, duration_ms));
            let resume_text = if can_resume {
                format_time(resume_ms.unwrap_or(0) as f64 / 1000.0)
            } else {
                String::new()
            };

            let performers = target_item
                .people
                .iter()
                .map(|p| p.name.clone().into())
                .collect::<Vec<slint::SharedString>>();

            ui.set_detail_id(target_item.id.clone().into());
            ui.set_detail_title(target_item.title.into());
            ui.set_detail_overview(overview_str.into());
            ui.set_detail_year(year_str.into());
            ui.set_detail_duration(duration_str.into());
            ui.set_detail_studio(studio_str.into());
            ui.set_detail_genres(genres_str.into());
            ui.set_detail_resolution(resolution_str.into());
            ui.set_detail_is_favorite(target_item.is_favorite);
            ui.set_detail_is_played(target_item.is_played);
            ui.set_detail_can_resume(can_resume);
            ui.set_detail_resume_text(resume_text.into());
            let perf_model = Rc::new(slint::VecModel::from(performers));
            ui.set_detail_performers(perf_model.into());

            // Stage 7: Similar Items ("More Like This")
            let all_items = cached_catalog.lock().unwrap().clone();
            let mut scored: Vec<(i32, TvItem)> = all_items
                .into_iter()
                .filter(|i| i.id != target_item.id)
                .map(|i| {
                    let mut score = 0;
                    for g in &target_item.genres {
                        if i.genres.iter().any(|ig| ig.eq_ignore_ascii_case(g)) {
                            score += 3;
                        }
                    }
                    for s in &target_item.studios {
                        if i.studios.iter().any(|is| is.eq_ignore_ascii_case(s)) {
                            score += 5;
                        }
                    }
                    for p in &target_item.people {
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
            let similar_cards: Vec<VideoCardData> = scored
                .into_iter()
                .filter(|(s, _)| *s > 0)
                .take(8)
                .map(|(_, item)| tv_item_to_card(&item))
                .collect();
            let sim_model = Rc::new(slint::VecModel::from(similar_cards));
            ui.set_detail_similar_items(sim_model.into());

            ui.set_detail_visible(true);
        });
    }

    // 2. Close Item Detail Modal
    {
        let weak = ui.as_weak();
        ui.on_close_item_detail(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_detail_visible(false);
            }
        });
    }

    // 3. Toggle Detail Favorite
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let credential_store = credential_store.clone();
        let jellyfin = jellyfin.clone();
        ui.on_toggle_detail_favorite(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let id = id.to_string();
            let new_fav = !ui.get_detail_is_favorite();
            ui.set_detail_is_favorite(new_fav);

            let mut list = cached_catalog.lock().unwrap();
            if let Some(item) = list.iter_mut().find(|i| i.id == id) {
                item.is_favorite = new_fav;
            }
            let cards = list.iter().map(tv_item_to_card).collect::<Vec<_>>();
            drop(list);
            set_catalog(&ui, cards);

            let session = credential_store.get().ok().flatten();
            if let Some(sess) = session {
                let jf = jellyfin.clone();
                let item_id = id.clone();
                std::thread::spawn(move || {
                    let _ = jf.toggle_favorite(
                        &sess.server_url,
                        &sess.user_id,
                        &sess.access_token,
                        &item_id,
                        new_fav,
                    );
                });
            }
        });
    }

    // 4. Toggle Detail Played
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let credential_store = credential_store.clone();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        ui.on_toggle_detail_played(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let id = id.to_string();
            let new_played = !ui.get_detail_is_played();
            ui.set_detail_is_played(new_played);

            let mut list = cached_catalog.lock().unwrap();
            if let Some(item) = list.iter_mut().find(|i| i.id == id) {
                item.is_played = new_played;
            }
            let cards = list.iter().map(tv_item_to_card).collect::<Vec<_>>();
            drop(list);
            set_catalog(&ui, cards);

            let _ = local_store.save_progress("local", &id, 0, 0);
            if id.starts_with("telegram:") {
                let prov_id = id.strip_prefix("telegram:").unwrap_or(&id);
                let _ = local_store.mark_telegram_played(prov_id, new_played, None, None, None);
            }

            let session = credential_store.get().ok().flatten();
            if let Some(sess) = session {
                let jf = jellyfin.clone();
                let item_id = id.clone();
                std::thread::spawn(move || {
                    let _ = jf.toggle_played(
                        &sess.server_url,
                        &sess.user_id,
                        &sess.access_token,
                        &item_id,
                        new_played,
                    );
                });
            }
        });
    }

    // 5. Play Detail Item (Play from start or resume)
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

        ui.on_play_detail_item(move |id, resume| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_detail_visible(false);
            let Some(player) = &player else {
                show_error(&ui, "libmpv is not available");
                return;
            };
            let id = id.to_string();

            let target_item = cached_catalog
                .lock()
                .unwrap()
                .iter()
                .find(|i| i.id == id)
                .cloned()
                .unwrap_or_else(|| {
                    if id == "sample" {
                        sample_catalog_tv_items()[0].clone()
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

            start_item_playback_with_resume(
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
                Some(resume),
            );
        });
    }

    // 6. Select Performer (Drilldown discovery)
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let credential_store = credential_store.clone();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        ui.on_select_performer(move |performer| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_detail_visible(false);
            ui.set_search_query(performer.clone());
            ui.set_active_screen("browse".into());
            let perf = performer.to_string();
            let weak = weak.clone();
            let cached_catalog = cached_catalog.clone();
            let credential_store = credential_store.clone();
            let jellyfin = jellyfin.clone();
            let local_store = local_store.clone();

            std::thread::spawn(move || {
                let session = credential_store.get().ok().flatten();
                let raw_items = if let Some(session) = session {
                    let q = GetItemsQuery {
                        person: Some(&perf),
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
                                .filter(|i| {
                                    i.people.iter().any(|p| p.name.eq_ignore_ascii_case(&perf))
                                })
                                .collect()
                        })
                } else {
                    sample_catalog_tv_items()
                        .into_iter()
                        .filter(|i| i.people.iter().any(|p| p.name.eq_ignore_ascii_case(&perf)))
                        .collect()
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

    // 7. Select Studio (Drilldown discovery)
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let credential_store = credential_store.clone();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        ui.on_select_studio(move |studio| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_detail_visible(false);
            ui.set_search_query(studio.clone());
            ui.set_active_screen("browse".into());
            let stud = studio.to_string();
            let weak = weak.clone();
            let cached_catalog = cached_catalog.clone();
            let credential_store = credential_store.clone();
            let jellyfin = jellyfin.clone();
            let local_store = local_store.clone();

            std::thread::spawn(move || {
                let session = credential_store.get().ok().flatten();
                let raw_items = if let Some(session) = session {
                    let q = GetItemsQuery {
                        studio: Some(&stud),
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
                                .filter(|i| i.studios.iter().any(|s| s.eq_ignore_ascii_case(&stud)))
                                .collect()
                        })
                } else {
                    sample_catalog_tv_items()
                        .into_iter()
                        .filter(|i| i.studios.iter().any(|s| s.eq_ignore_ascii_case(&stud)))
                        .collect()
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
}
