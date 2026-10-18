use crate::catalog::{
    filter_catalog_items, sample_catalog_tv_items, set_catalog, tv_item_to_card,
    tv_item_to_card_for_server,
};
use crate::context::AppContext;
use crate::playback::{
    format_time, show_error, start_item_playback, start_item_playback_with_resume,
};
use crate::session::PlaybackSessionController;
use crate::{MainWindow, VideoCardData};
use adult_hub_client::{GetItemsQuery, JellyfinClient, TvItem};
use slint::{ComponentHandle, Model};
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
    let home_catalog = ctx.home_catalog.clone();
    let phash_coord = ctx.phash_coord.clone();

    // 1. Open Item Detail Modal
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let home_catalog = home_catalog.clone();
        let local_store = local_store.clone();
        let session_ctrl = session_ctrl.clone();
        let credential_store = credential_store.clone();
        let jellyfin = jellyfin.clone();
        ui.on_open_item_detail(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let id = id.to_string();
            let target_item = cached_catalog
                .lock()
                .unwrap()
                .iter()
                .find(|i| i.id == id)
                .cloned()
                .or_else(|| {
                    home_catalog
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|item| item.id == id)
                        .cloned()
                });
            let target_item = match target_item {
                Some(item) => item,
                None if id == "sample" => sample_catalog_tv_items()[0].clone(),
                None => {
                    show_error(
                        &ui,
                        "This item is no longer in the current catalog. Refresh and try again.",
                    );
                    return;
                }
            };

            if target_item
                .genres
                .iter()
                .any(|tag| tag == "__adult_hub_genre")
            {
                ui.set_detail_visible(false);
                ui.set_active_screen("browse".into());
                ui.set_catalog_category_drilldown(true);
                ui.set_active_category("Latest".into());
                ui.set_search_query("".into());
                ui.set_catalog_genre_query(target_item.title.clone().into());
                ui.invoke_set_catalog_text_filter("genre".into(), target_item.title.into());
                return;
            }

            if target_item
                .genres
                .iter()
                .any(|tag| tag == "__adult_hub_performer")
            {
                ui.invoke_open_performer_profile(target_item.title.into());
                return;
            }
            if target_item
                .genres
                .iter()
                .any(|tag| tag == "__adult_hub_studio")
            {
                ui.invoke_open_studio_profile(target_item.title.into());
                return;
            }

            let is_telegram_item = target_item.is_telegram();
            let year_str = target_item.year.map(|y| y.to_string()).unwrap_or_default();
            let duration_str = target_item.formatted_duration();
            let resolution_str = match (target_item.width, target_item.height) {
                (Some(w), Some(h)) => format!("{w}x{h}"),
                _ => String::new(),
            };
            let studio_str = target_item.studios.first().cloned().unwrap_or_default();
            let genres_str = target_item.genres.join(", ");
            let overview_str = target_item.overview.clone().unwrap_or_default();

            // Use any already-cached artwork immediately, then fetch it off the UI thread.
            let artwork_session = credential_store.get().ok().flatten();
            let detail_card = artwork_session.as_ref().map_or_else(
                || tv_item_to_card(&target_item),
                |session| tv_item_to_card_for_server(&target_item, &session.server_url),
            );
            let artwork_url = detail_card.thumbnail_url.to_string();
            let artwork_server = artwork_session
                .as_ref()
                .map(|session| session.server_url.clone());
            let artwork_token = artwork_session
                .as_ref()
                .map(|session| session.access_token.clone());
            let backdrop_url = if is_telegram_item {
                crate::thumbnails::phash_thumbnail_url(&target_item.id)
            } else {
                target_item.backdrop_image_tag.as_deref().and_then(|tag| {
                    artwork_server.as_ref().map(|server| {
                        JellyfinClient::image_url(
                            server,
                            &target_item.id,
                            "Backdrop/0",
                            1920,
                            Some(tag),
                        )
                    })
                })
            };
            let cached_backdrop = backdrop_url
                .as_deref()
                .filter(|url| !crate::thumbnails::is_poor_thumbnail(url))
                .and_then(crate::thumbnails::load_cached_image);

            // Calculate resume information
            let duration_ms = target_item
                .runtime_ticks
                .map(|t| (t / 10_000) as u64)
                .unwrap_or(0);
            let resume_ms = if is_telegram_item {
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
            let genre_items = target_item
                .genres
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<slint::SharedString>>();
            let tag_items = target_item
                .tags
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<slint::SharedString>>();

            let mut performer_image_jobs = Vec::new();
            let performer_cards = target_item
                .people
                .iter()
                .filter(|person| {
                    person
                        .person_type
                        .as_deref()
                        .is_none_or(|kind| kind.eq_ignore_ascii_case("actor"))
                })
                .take(20)
                .enumerate()
                .map(|(index, person)| {
                    let image_url = artwork_server.as_ref().map(|server| {
                        JellyfinClient::image_url(
                            server,
                            &person.id,
                            "Primary",
                            160,
                            person.primary_image_tag.as_deref(),
                        )
                    });
                    let cached_image = image_url
                        .as_deref()
                        .and_then(crate::thumbnails::load_cached_image);
                    if let Some(url) = image_url.as_ref()
                        && cached_image.is_none()
                    {
                        performer_image_jobs.push((index, url.clone()));
                    }
                    crate::PerformerCardData {
                        id: person.id.clone().into(),
                        name: person.name.clone().into(),
                        role: person.role.as_deref().unwrap_or("Actor").into(),
                        has_image: cached_image.is_some(),
                        image: cached_image.unwrap_or_default(),
                    }
                })
                .collect::<Vec<_>>();

            ui.set_detail_id(target_item.id.clone().into());
            ui.set_detail_title(target_item.title.into());
            ui.set_detail_overview(overview_str.into());
            ui.set_detail_year(year_str.into());
            ui.set_detail_duration(duration_str.into());
            ui.set_detail_studio(studio_str.into());
            ui.set_detail_genres(genres_str.into());
            ui.set_detail_genre_items(Rc::new(slint::VecModel::from(genre_items)).into());
            ui.set_detail_tag_items(Rc::new(slint::VecModel::from(tag_items)).into());
            ui.set_detail_resolution(resolution_str.into());
            ui.set_detail_thumbnail(detail_card.thumbnail.clone());
            ui.set_detail_has_thumbnail(detail_card.has_thumbnail);
            ui.set_detail_backdrop(cached_backdrop.clone().unwrap_or_default());
            ui.set_detail_has_backdrop(cached_backdrop.is_some());
            ui.set_detail_is_favorite(target_item.is_favorite);
            ui.set_detail_is_played(target_item.is_played);
            ui.set_detail_can_resume(can_resume);
            ui.set_detail_resume_text(resume_text.into());
            let perf_model = Rc::new(slint::VecModel::from(performers));
            ui.set_detail_performers(perf_model.into());
            ui.set_detail_performer_cards(Rc::new(slint::VecModel::from(performer_cards)).into());

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
            ui.set_detail_similar_loading(!is_telegram_item && artwork_session.is_some());

            ui.set_detail_visible(true);

            if cached_backdrop.is_none()
                && let Some(backdrop_url) = backdrop_url
            {
                let weak = weak.clone();
                let item_id = target_item.id.clone();
                let token = artwork_token.clone();
                std::thread::spawn(move || {
                    let Some(image_bytes) = crate::thumbnails::fetch_and_cache_image_bytes(
                        &backdrop_url,
                        token.as_deref(),
                    ) else {
                        return;
                    };
                    // Reject empty/black Jellyfin artwork just like Android TV. The
                    // detail poster remains visible underneath as the fallback.
                    if crate::thumbnails::is_poor_thumbnail_bytes(&image_bytes) {
                        return;
                    }
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade()
                            && ui.get_detail_id() == item_id.as_str()
                            && let Some(image) = crate::thumbnails::image_from_bytes(&image_bytes)
                        {
                            ui.set_detail_backdrop(image);
                            ui.set_detail_has_backdrop(true);
                        }
                    });
                });
            }

            let needs_artwork_refresh = (!detail_card.fallback_thumbnail_url.is_empty()
                && is_telegram_item)
                || (!detail_card.has_thumbnail && !artwork_url.is_empty());
            if needs_artwork_refresh {
                let weak = weak.clone();
                let item_id = target_item.id.clone();
                let token = artwork_token.clone();
                let thumbnail_request = crate::catalog::ThumbnailRequest::from(&detail_card);
                std::thread::spawn(move || {
                    let Some(thumbnail) =
                        crate::catalog::fetch_card_thumbnail(&thumbnail_request, token.as_deref())
                    else {
                        return;
                    };
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade()
                            && ui.get_detail_id() == item_id.as_str()
                            && let Some(image) =
                                crate::thumbnails::image_from_bytes(&thumbnail.bytes)
                        {
                            ui.set_detail_thumbnail(image);
                            ui.set_detail_has_thumbnail(true);
                        }
                    });
                });
            }

            if !performer_image_jobs.is_empty() {
                let weak = weak.clone();
                let item_id = target_item.id.clone();
                let token = artwork_token.clone();
                std::thread::spawn(move || {
                    for (index, image_url) in performer_image_jobs {
                        let Some(image_bytes) = crate::thumbnails::fetch_and_cache_image_bytes(
                            &image_url,
                            token.as_deref(),
                        ) else {
                            continue;
                        };
                        let weak = weak.clone();
                        let item_id = item_id.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade()
                                && ui.get_detail_id() == item_id.as_str()
                                && let Some(image) =
                                    crate::thumbnails::image_from_bytes(&image_bytes)
                            {
                                let model = ui.get_detail_performer_cards();
                                let mut cards = (0..model.row_count())
                                    .filter_map(|row| model.row_data(row))
                                    .collect::<Vec<_>>();
                                if let Some(card) = cards.get_mut(index) {
                                    card.image = image;
                                    card.has_image = true;
                                    ui.set_detail_performer_cards(
                                        Rc::new(slint::VecModel::from(cards)).into(),
                                    );
                                }
                            }
                        });
                    }
                });
            }

            if !is_telegram_item {
                if let Some(session) = artwork_session {
                    let weak = weak.clone();
                    let detail_id = target_item.id.clone();
                    let jellyfin = jellyfin.clone();
                    let local_store = local_store.clone();
                    std::thread::spawn(move || {
                        let similar_result = jellyfin.get_similar_items(
                            &session.server_url,
                            &session.access_token,
                            &detail_id,
                            12,
                        );
                        let similar_items = match similar_result {
                            Ok(items) => filter_catalog_items(items, &local_store),
                            Err(_) => {
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = weak.upgrade()
                                        && ui.get_detail_id() == detail_id.as_str()
                                    {
                                        ui.set_detail_similar_loading(false);
                                    }
                                });
                                return;
                            }
                        };

                        let weak = weak.clone();
                        let detail_id_for_ui = detail_id.clone();
                        let server_url = session.server_url.clone();
                        let token = session.access_token.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade()
                                && ui.get_detail_id() == detail_id_for_ui.as_str()
                            {
                                let cards = similar_items
                                    .iter()
                                    .map(|item| tv_item_to_card_for_server(item, &server_url))
                                    .collect::<Vec<_>>();
                                let image_jobs = cards
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, card)| {
                                        !card.has_thumbnail
                                            || (card.provider == "telegram"
                                                && !card.fallback_thumbnail_url.is_empty())
                                    })
                                    .map(|(index, card)| {
                                        (index, crate::catalog::ThumbnailRequest::from(card))
                                    })
                                    .collect::<Vec<_>>();
                                ui.set_detail_similar_items(
                                    Rc::new(slint::VecModel::from(cards)).into(),
                                );
                                ui.set_detail_similar_loading(false);

                                if !image_jobs.is_empty() {
                                    let weak = weak.clone();
                                    let detail_id = detail_id_for_ui.clone();
                                    std::thread::spawn(move || {
                                        for (index, request) in image_jobs {
                                            let Some(thumbnail) =
                                                crate::catalog::fetch_card_thumbnail(
                                                    &request,
                                                    Some(&token),
                                                )
                                            else {
                                                continue;
                                            };
                                            let weak = weak.clone();
                                            let detail_id = detail_id.clone();
                                            let _ = slint::invoke_from_event_loop(move || {
                                                if let Some(ui) = weak.upgrade()
                                                    && ui.get_detail_id() == detail_id.as_str()
                                                    && let Some(image) =
                                                        crate::thumbnails::image_from_bytes(
                                                            &thumbnail.bytes,
                                                        )
                                                {
                                                    let model = ui.get_detail_similar_items();
                                                    let mut cards = (0..model.row_count())
                                                        .filter_map(|row| model.row_data(row))
                                                        .collect::<Vec<_>>();
                                                    if let Some(card) = cards.get_mut(index) {
                                                        card.thumbnail = image;
                                                        card.thumbnail_url = thumbnail.url.into();
                                                        card.has_thumbnail = true;
                                                        ui.set_detail_similar_items(
                                                            Rc::new(slint::VecModel::from(cards))
                                                                .into(),
                                                        );
                                                    }
                                                }
                                            });
                                        }
                                    });
                                }
                            }
                        });
                    });
                }
            }
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
                let weak = weak.clone();
                let cached_catalog = cached_catalog.clone();
                std::thread::spawn(move || {
                    if jf
                        .toggle_favorite(
                            &sess.server_url,
                            &sess.user_id,
                            &sess.access_token,
                            &item_id,
                            new_fav,
                        )
                        .is_err()
                    {
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.set_detail_is_favorite(!new_fav);
                                let mut items = cached_catalog.lock().unwrap();
                                if let Some(item) = items.iter_mut().find(|item| item.id == item_id)
                                {
                                    item.is_favorite = !new_fav;
                                }
                                set_catalog(&ui, items.iter().map(tv_item_to_card).collect());
                                ui.set_status("Could not update favorite state on Jellyfin".into());
                                ui.set_status_color(slint::Color::from_rgb_u8(244, 67, 54));
                            }
                        });
                    }
                });
            } else {
                ui.set_detail_is_favorite(!new_fav);
                if let Some(item) = cached_catalog
                    .lock()
                    .unwrap()
                    .iter_mut()
                    .find(|item| item.id == id)
                {
                    item.is_favorite = !new_fav;
                }
                ui.set_status("Sign in to Jellyfin to change favorites".into());
                ui.set_status_color(slint::Color::from_rgb_u8(244, 67, 54));
            }
        });
    }

    // Favorite the currently playing Jellyfin item without leaving the player.
    {
        let weak = ui.as_weak();
        let current_item = current_item.clone();
        let cached_catalog = cached_catalog.clone();
        let home_catalog = home_catalog.clone();
        let credential_store = credential_store.clone();
        let jellyfin = jellyfin.clone();
        ui.on_toggle_player_favorite(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(mut item) = current_item.lock().unwrap().clone() else {
                show_error(&ui, "No active Jellyfin item to favorite");
                return;
            };
            if item.is_telegram() || uuid::Uuid::parse_str(&item.id).is_err() {
                ui.set_status("Favorites are available for Jellyfin library items".into());
                ui.set_status_color(slint::Color::from_rgb_u8(244, 177, 66));
                return;
            }
            let Some(session) = credential_store.get().ok().flatten() else {
                show_error(&ui, "Sign in to Jellyfin to change favorites");
                return;
            };

            let new_favorite = !item.is_favorite;
            item.is_favorite = new_favorite;
            *current_item.lock().unwrap() = Some(item.clone());
            for catalog in [&cached_catalog, &home_catalog] {
                if let Some(cached) = catalog
                    .lock()
                    .unwrap()
                    .iter_mut()
                    .find(|cached| cached.id == item.id)
                {
                    cached.is_favorite = new_favorite;
                }
            }
            ui.set_player_is_favorite(new_favorite);
            {
                let catalog = cached_catalog.lock().unwrap();
                set_catalog(&ui, catalog.iter().map(tv_item_to_card).collect());
            }

            let weak = weak.clone();
            let current_item = current_item.clone();
            let cached_catalog = cached_catalog.clone();
            let home_catalog = home_catalog.clone();
            let jellyfin = jellyfin.clone();
            let item_id = item.id.clone();
            std::thread::spawn(move || {
                if jellyfin
                    .toggle_favorite(
                        &session.server_url,
                        &session.user_id,
                        &session.access_token,
                        &item.id,
                        new_favorite,
                    )
                    .is_ok()
                {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_status(
                                if new_favorite {
                                    "Added to favorites"
                                } else {
                                    "Removed from favorites"
                                }
                                .into(),
                            );
                            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                            ui.invoke_refresh_home();
                        }
                    });
                    return;
                }

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        let mut current_matches = false;
                        if let Some(item) = current_item.lock().unwrap().as_mut() {
                            if item.id == item_id {
                                item.is_favorite = !new_favorite;
                                current_matches = true;
                            }
                        }
                        if current_matches {
                            ui.set_player_is_favorite(!new_favorite);
                        }
                        for catalog in [&cached_catalog, &home_catalog] {
                            if let Some(item) = catalog
                                .lock()
                                .unwrap()
                                .iter_mut()
                                .find(|item| item.id == item_id)
                            {
                                item.is_favorite = !new_favorite;
                            }
                        }
                        let catalog = cached_catalog.lock().unwrap();
                        set_catalog(&ui, catalog.iter().map(tv_item_to_card).collect());
                        ui.set_status("Could not update favorite state on Jellyfin".into());
                        ui.set_status_color(slint::Color::from_rgb_u8(244, 67, 54));
                        ui.invoke_refresh_home();
                    }
                });
            });
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
                .cloned();
            let target_item = match target_item {
                Some(item) => item,
                None if id == "sample" => sample_catalog_tv_items()[0].clone(),
                None => {
                    show_error(
                        &ui,
                        "This item is no longer in the current catalog. Refresh and try again.",
                    );
                    return;
                }
            };

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
        ui.on_play_random_contextual(move |mode| {
            let Some(ui) = weak.upgrade() else { return };
            let catalog = cached_catalog.lock().unwrap().clone();
            let current_id = ui.get_detail_id().to_string();
            let Some(current) = catalog.iter().find(|item| item.id == current_id) else {
                show_error(
                    &ui,
                    "Open a video detail page before using contextual shuffle.",
                );
                return;
            };

            let is_video_item = |item: &&TvItem| {
                item.genres
                    .iter()
                    .all(|tag| !tag.starts_with("__adult_hub_"))
                    && item.id != "sample"
            };
            let mut candidates = catalog
                .iter()
                .filter(is_video_item)
                .filter(|item| item.id != current_id)
                .filter(|item| match mode.as_str() {
                    "unwatched" => !item.is_played,
                    "performer" => current.people.iter().any(|person| {
                        person
                            .person_type
                            .as_deref()
                            .is_none_or(|kind| kind.eq_ignore_ascii_case("actor"))
                            && item
                                .people
                                .iter()
                                .any(|candidate| candidate.name.eq_ignore_ascii_case(&person.name))
                    }),
                    "genre" => current
                        .genres
                        .iter()
                        .filter(|genre| !genre.starts_with("__adult_hub_"))
                        .any(|genre| {
                            item.genres
                                .iter()
                                .any(|candidate| candidate.eq_ignore_ascii_case(genre))
                        }),
                    _ => true,
                })
                .cloned()
                .collect::<Vec<_>>();

            if candidates.is_empty() && mode != "default" {
                candidates = catalog
                    .iter()
                    .filter(is_video_item)
                    .filter(|item| item.id != current_id)
                    .cloned()
                    .collect();
            }
            if candidates.is_empty() {
                show_error(&ui, "No other catalog videos are available for shuffle.");
                return;
            }
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            let index = (nonce % candidates.len() as u128) as usize;
            let target = candidates.swap_remove(index);
            ui.set_detail_visible(false);
            let Some(player) = &player else {
                show_error(&ui, "libmpv is not available");
                return;
            };
            start_item_playback(
                &ui,
                player,
                target,
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
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };

                let raw_items = filter_catalog_items(raw_items, &local_store);

                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        *cached_catalog.lock().unwrap() = raw_items.clone();
                        let cards = raw_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
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
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };

                let raw_items = filter_catalog_items(raw_items, &local_store);
                *cached_catalog.lock().unwrap() = raw_items.clone();

                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        let cards = raw_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
                        set_catalog(&ui, cards);
                    }
                })
                .ok();
            });
        });
    }
}
