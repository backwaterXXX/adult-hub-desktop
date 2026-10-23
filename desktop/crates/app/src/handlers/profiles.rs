use crate::MainWindow;
use crate::catalog::{filter_catalog_items, tv_item_to_card, tv_item_to_card_for_server};
use crate::context::AppContext;
use adult_hub_client::{GetItemsQuery, JellyfinClient, TvItem};
use slint::{ComponentHandle, Model};
use std::rc::Rc;

fn profile_cards(items: &[TvItem], server_url: Option<&str>) -> Vec<crate::VideoCardData> {
    items
        .iter()
        .map(|item| {
            server_url.map_or_else(
                || tv_item_to_card(item),
                |server_url| tv_item_to_card_for_server(item, server_url),
            )
        })
        .collect()
}

fn fetch_cards_thumbnails(
    weak: slint::Weak<MainWindow>,
    target_kind: &'static str,
    expected_name: String,
    cards: Vec<crate::VideoCardData>,
    token: String,
) {
    let jobs = cards
        .iter()
        .filter(|card| {
            !card.has_thumbnail
                || (card.provider == "telegram" && !card.fallback_thumbnail_url.is_empty())
        })
        .map(|card| {
            (
                card.id.clone(),
                crate::catalog::ThumbnailRequest::from(card),
            )
        })
        .collect::<Vec<_>>();
    if jobs.is_empty() {
        return;
    }

    std::thread::spawn(move || {
        for (expected_id, card) in jobs {
            let Some(thumbnail) = crate::catalog::fetch_card_thumbnail(&card, Some(&token)) else {
                continue;
            };
            let weak = weak.clone();
            let expected_name = expected_name.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak.upgrade() else { return };
                let matches_name = if target_kind == "performer" {
                    ui.get_performer_name() == expected_name.as_str()
                } else {
                    ui.get_studio_name() == expected_name.as_str()
                };
                if !matches_name {
                    return;
                }
                if let Some(image) = crate::thumbnails::image_from_bytes(&thumbnail.bytes) {
                    let model = if target_kind == "performer" {
                        ui.get_performer_items()
                    } else {
                        ui.get_studio_items()
                    };
                    let mut items = (0..model.row_count())
                        .filter_map(|row| model.row_data(row))
                        .collect::<Vec<_>>();
                    // Attach by stable record id, never by the index captured
                    // when the batch started: the model may have been rebuilt.
                    if let Some(pos) = items
                        .iter()
                        .position(|item| item.id == expected_id.as_str())
                    {
                        let item = &mut items[pos];
                        item.thumbnail = image;
                        item.thumbnail_url = thumbnail.url.into();
                        item.has_thumbnail = true;
                        let new_model = Rc::new(slint::VecModel::from(items));
                        if target_kind == "performer" {
                            ui.set_performer_items(new_model.into());
                        } else {
                            ui.set_studio_items(new_model.into());
                        }
                    }
                }
            });
        }
    });
}

fn load_performer_profile_data(ui: &MainWindow, ctx: &AppContext, p_name: String) {
    let weak = ui.as_weak();
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let jellyfin = ctx.jellyfin.clone();
    let resolver = ctx.resolver.clone();
    let cached_catalog = ctx.cached_catalog.clone();
    let home_catalog = ctx.home_catalog.clone();
    let performer_catalog = ctx.performer_catalog.clone();

    // 1. Find matching local items
    let mut local_items = Vec::new();
    {
        let cached = cached_catalog.lock().unwrap();
        let home = home_catalog.lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        for item in cached.iter().chain(home.iter()) {
            if item
                .people
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&p_name))
                && seen.insert(item.id.clone())
            {
                local_items.push(item.clone());
            }
        }
    }

    let session = credential_store.get().ok().flatten();
    let server_url = session.as_ref().map(|s| s.server_url.clone());
    let token = session.as_ref().map(|s| s.access_token.clone());

    std::thread::spawn(move || {
        let mut person_image_bytes = None;
        let mut person_child_count = None;
        let mut person_overview = None;

        // 2. Query dedicated Jellyfin Person endpoint: GET /Persons/{name}?userId={userId}
        if let Some(session) = session.as_ref() {
            if let Ok(details) = jellyfin.get_person(
                &session.server_url,
                &session.user_id,
                &session.access_token,
                &p_name,
            ) {
                person_overview = details.overview;
                person_child_count = details.child_count;
                if let Some(tag) = details.primary_image_tag.as_deref() {
                    let image_url = JellyfinClient::image_url(
                        &session.server_url,
                        &details.id,
                        "Primary",
                        crate::thumbnails::person_thumbnail_max_width(),
                        Some(tag),
                    );
                    person_image_bytes = crate::thumbnails::load_cached_image_bytes(&image_url)
                        .or_else(|| {
                            crate::thumbnails::fetch_and_cache_image_bytes(
                                &image_url,
                                Some(&session.access_token),
                            )
                        });
                }
            }
        }

        // 3. Query resolver for bio, nationality, birth, aliases
        let res_url = local_store
            .get_setting("resolver_base_url")
            .ok()
            .flatten()
            .or_else(|| server_url.clone())
            .unwrap_or_default();
        let bio_info = if !res_url.is_empty() {
            resolver.get_performer_bio(&res_url, &p_name).ok()
        } else {
            None
        };

        // 4. Query filmography from Jellyfin
        let remote_result = if let Some(session) = session.as_ref() {
            let q = GetItemsQuery {
                person: Some(&p_name),
                limit: 100,
                include_item_types: Some("Movie,Video"),
                sort_by: "SortName",
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
        } else {
            Ok(Vec::new())
        };

        let is_remote_err = remote_result.is_err();
        let remote_err_msg = remote_result.as_ref().err().map(|e| e.to_string());
        let remote_items = remote_result.unwrap_or_default();

        // Merge remote + local items deduplicating by id
        let mut merged = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for item in remote_items.into_iter().chain(local_items) {
            if seen.insert(item.id.clone()) {
                merged.push(item);
            }
        }
        let filtered_items = filter_catalog_items(merged, &local_store);
        *performer_catalog.lock().unwrap() = filtered_items.clone();

        // 5. Update UI on event loop
        let p_name_clone = p_name.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_performer_name() != p_name_clone.as_str() {
                return;
            }

            ui.set_performer_loading(false);

            // Update photo
            if let Some(bytes) = person_image_bytes.as_deref()
                && let Some(image) = crate::thumbnails::image_from_bytes(bytes)
            {
                ui.set_performer_image(image);
                ui.set_performer_has_image(true);
            }

            // Update bio
            let mut resolved_bio = String::new();
            if let Some(bio) = bio_info.as_ref().and_then(|b| b.bio.as_ref()) {
                resolved_bio = bio.clone();
            } else if let Some(ov) = person_overview.filter(|o| !o.trim().is_empty()) {
                resolved_bio = ov;
            }
            if !resolved_bio.is_empty() {
                // The collapsed bio view clamps to five lines; "Show more"
                // should only appear once the text is actually shortened.
                // At ~76 characters per wrapped line this is the ~380-char
                // boundary, with a margin for short words and spacing.
                let is_long = resolved_bio.chars().count() > 380;
                ui.set_performer_bio_long(is_long);
                ui.set_performer_bio(resolved_bio.into());
            }

            if let Some(bio) = bio_info {
                if let Some(nat) = bio.nationality {
                    ui.set_performer_nationality(nat.into());
                }
                if let Some(birth) = bio.birth {
                    ui.set_performer_birth(birth.into());
                }
                if !bio.aliases.is_empty() {
                    ui.set_performer_aliases(bio.aliases.join(", ").into());
                }
            }

            // Filmography results
            if filtered_items.is_empty() && is_remote_err {
                let err_msg =
                    remote_err_msg.unwrap_or_else(|| "Failed to load filmography".to_string());
                ui.set_performer_error(format!("Could not load filmography: {err_msg}").into());
                ui.set_performer_movie_count(0);
                ui.set_performer_items(Rc::new(slint::VecModel::default()).into());
            } else {
                ui.set_performer_error("".into());
                let count = if !filtered_items.is_empty() {
                    filtered_items.len() as i32
                } else {
                    person_child_count.unwrap_or(0) as i32
                };
                ui.set_performer_movie_count(count);
                let cards = profile_cards(&filtered_items, server_url.as_deref());
                ui.set_performer_items(Rc::new(slint::VecModel::from(cards.clone())).into());

                if let Some(token) = token {
                    fetch_cards_thumbnails(weak.clone(), "performer", p_name_clone, cards, token);
                }
            }
        });
    });
}

fn load_studio_profile_data(ui: &MainWindow, ctx: &AppContext, s_name: String) {
    let weak = ui.as_weak();
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let jellyfin = ctx.jellyfin.clone();
    let cached_catalog = ctx.cached_catalog.clone();
    let home_catalog = ctx.home_catalog.clone();
    let studio_catalog = ctx.studio_catalog.clone();

    // 1. Find matching local items
    let mut local_items = Vec::new();
    {
        let cached = cached_catalog.lock().unwrap();
        let home = home_catalog.lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        for item in cached.iter().chain(home.iter()) {
            if item.studios.iter().any(|s| s.eq_ignore_ascii_case(&s_name))
                && seen.insert(item.id.clone())
            {
                local_items.push(item.clone());
            }
        }
    }

    let session = credential_store.get().ok().flatten();
    let server_url = session.as_ref().map(|s| s.server_url.clone());
    let token = session.as_ref().map(|s| s.access_token.clone());

    std::thread::spawn(move || {
        // Query studio items from Jellyfin
        let remote_result = if let Some(session) = session.as_ref() {
            let q = GetItemsQuery {
                studio: Some(&s_name),
                limit: 100,
                include_item_types: Some("Movie,Video"),
                sort_by: "SortName",
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
        } else {
            Ok(Vec::new())
        };

        let is_remote_err = remote_result.is_err();
        let remote_err_msg = remote_result.as_ref().err().map(|e| e.to_string());
        let remote_items = remote_result.unwrap_or_default();

        let mut merged = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for item in remote_items.into_iter().chain(local_items) {
            if seen.insert(item.id.clone()) {
                merged.push(item);
            }
        }
        let filtered_items = filter_catalog_items(merged, &local_store);
        *studio_catalog.lock().unwrap() = filtered_items.clone();

        let s_name_clone = s_name.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_studio_name() != s_name_clone.as_str() {
                return;
            }

            ui.set_studio_loading(false);

            if filtered_items.is_empty() && is_remote_err {
                let err_msg =
                    remote_err_msg.unwrap_or_else(|| "Failed to load studio catalog".to_string());
                ui.set_studio_error(format!("Could not load studio catalog: {err_msg}").into());
                ui.set_studio_items(Rc::new(slint::VecModel::default()).into());
            } else {
                ui.set_studio_error("".into());
                let cards = profile_cards(&filtered_items, server_url.as_deref());
                ui.set_studio_items(Rc::new(slint::VecModel::from(cards.clone())).into());

                if let Some(token) = token {
                    fetch_cards_thumbnails(weak.clone(), "studio", s_name_clone, cards, token);
                }
            }
        });
    });
}

pub fn install_stage7_handlers(ui: &MainWindow, ctx: &AppContext) {
    // 1. Open Performer Profile Screen
    {
        let weak = ui.as_weak();
        let ctx_prof = ctx.clone();
        ui.on_open_performer_profile(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            ui.invoke_cache_catalog_view();
            ctx_prof.push_nav_destination(crate::handlers::browsing::current_nav_destination(&ui));
            let name_str = name.to_string();
            ui.set_performer_name(name);
            ui.set_performer_scroll_y(0.0);
            ui.set_performer_anchor_index(0);
            ui.set_performer_anchor_screen_y(0.0);
            ui.set_performer_anchor_pending(false);
            ui.set_performer_loading(true);
            ui.set_performer_error("".into());
            ui.set_performer_movie_count(-1);
            ui.set_performer_has_image(false);
            ui.set_performer_image(slint::Image::default());
            ui.set_performer_bio("".into());
            ui.set_performer_bio_long(false);
            ui.set_performer_bio_expanded(false);
            ui.set_performer_nationality("".into());
            ui.set_performer_birth("".into());
            ui.set_performer_aliases("".into());
            ui.set_performer_items(Rc::new(slint::VecModel::default()).into());
            ui.set_active_screen("performer-details".into());

            load_performer_profile_data(&ui, &ctx_prof, name_str);
        });
    }

    // 2. Retry Performer Profile
    {
        let weak = ui.as_weak();
        let ctx_retry = ctx.clone();
        ui.on_retry_performer_profile(move || {
            let Some(ui) = weak.upgrade() else { return };
            let name_str = ui.get_performer_name().to_string();
            if name_str.is_empty() {
                return;
            }
            ui.set_performer_loading(true);
            ui.set_performer_error("".into());
            load_performer_profile_data(&ui, &ctx_retry, name_str);
        });
    }

    // 3. Close Performer Profile / Navigate Back
    {
        let weak = ui.as_weak();
        ui.on_close_performer_profile(move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_navigate_back();
            }
        });
    }

    // 4. Open Studio Profile Screen
    {
        let weak = ui.as_weak();
        let ctx_prof = ctx.clone();
        ui.on_open_studio_profile(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            ui.invoke_cache_catalog_view();
            ctx_prof.push_nav_destination(crate::handlers::browsing::current_nav_destination(&ui));
            let studio_str = name.to_string();
            ui.set_studio_name(name);
            ui.set_studio_scroll_y(0.0);
            ui.set_studio_anchor_index(0);
            ui.set_studio_anchor_screen_y(0.0);
            ui.set_studio_anchor_pending(false);
            ui.set_studio_loading(true);
            ui.set_studio_error("".into());
            ui.set_studio_items(Rc::new(slint::VecModel::default()).into());
            ui.set_active_screen("studio-details".into());

            load_studio_profile_data(&ui, &ctx_prof, studio_str);
        });
    }

    // 5. Retry Studio Profile
    {
        let weak = ui.as_weak();
        let ctx_retry = ctx.clone();
        ui.on_retry_studio_profile(move || {
            let Some(ui) = weak.upgrade() else { return };
            let studio_str = ui.get_studio_name().to_string();
            if studio_str.is_empty() {
                return;
            }
            ui.set_studio_loading(true);
            ui.set_studio_error("".into());
            load_studio_profile_data(&ui, &ctx_retry, studio_str);
        });
    }

    // 6. Close Studio Profile / Navigate Back
    {
        let weak = ui.as_weak();
        ui.on_close_studio_profile(move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_navigate_back();
            }
        });
    }
}
