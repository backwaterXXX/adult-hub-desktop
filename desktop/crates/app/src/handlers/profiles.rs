use crate::MainWindow;
use crate::catalog::{filter_catalog_items, tv_item_to_card, tv_item_to_card_for_server};
use crate::context::AppContext;
use adult_hub_client::TvItem;
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

fn fetch_adult_hub_profile_items(
    client: &adult_hub_client::AdultHubClient,
    filter_type: &str,
    name_or_id: &str,
) -> Result<Vec<TvItem>, String> {
    let mut items = Vec::new();
    let mut offset = 0u64;
    let page_size = 100u64;
    let is_performer = filter_type == "performer";
    let is_studio = filter_type == "studio";
    let (performer_arg, performer_id_arg) = if is_performer {
        (None, Some(name_or_id))
    } else {
        (None, None)
    };
    let studio_arg = is_studio.then_some(name_or_id);

    loop {
        let req = adult_hub_client::CatalogVideosQuery {
            offset: Some(offset),
            limit: Some(page_size),
            sort: Some("title"),
            direction: Some("asc"),
            performer: performer_arg,
            performer_id: performer_id_arg,
            studio: studio_arg,
            ..Default::default()
        };
        let resp = client.get_videos(&req).map_err(|e| e.to_string())?;
        let page_len = resp.items.len();
        let total_count = resp.total_count;
        let has_more = resp.has_more;
        for video in resp.items {
            items.push(TvItem::from(video));
        }
        offset += page_len as u64;
        if !has_more || page_len == 0 || offset >= total_count {
            break;
        }
    }
    Ok(items)
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
                    let current_id = ui.get_performer_id();
                    if !current_id.is_empty() {
                        current_id == expected_name.as_str()
                    } else {
                        ui.get_performer_name() == expected_name.as_str()
                    }
                } else {
                    ui.get_studio_name() == expected_name.as_str()
                };
                if !matches_name {
                    return;
                }
                {
                    let image = crate::thumbnails::remember_artwork_image(
                        &thumbnail.url,
                        crate::thumbnails::CARD_ARTWORK_WIDTH,
                        &thumbnail.artwork,
                    );
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

fn load_performer_profile_data(ui: &MainWindow, ctx: &AppContext, p_key: String) {
    let weak = ui.as_weak();
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let cached_catalog = ctx.catalog.cached_catalog.clone();
    let home_catalog = ctx.catalog.home_catalog.clone();
    let performer_catalog = ctx.catalog.performer_catalog.clone();

    // 1. Find matching local items
    let mut local_items = Vec::new();
    {
        let cached = cached_catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = home_catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut seen = std::collections::HashSet::new();
        let is_name_ref = p_key.starts_with("name:");
        let unvetted_name = if is_name_ref {
            p_key.trim_start_matches("name:").trim()
        } else {
            p_key.as_str()
        };
        for item in cached.iter().chain(home.iter()) {
            if item.people.iter().any(|p| {
                if is_name_ref {
                    p.name.eq_ignore_ascii_case(unvetted_name)
                } else {
                    p.id == p_key || p.name.eq_ignore_ascii_case(&p_key)
                }
            }) && seen.insert(item.id.clone())
            {
                local_items.push(item.clone());
            }
        }
    }

    let session = credential_store.get().ok().flatten();
    let server_url = session.as_ref().map(|s| s.server_url.clone());
    let token = session.as_ref().map(|s| s.access_token.clone());

    std::thread::spawn(move || {
        let mut person_artwork: Option<(String, crate::thumbnails::DecodedArtwork)> = None;
        let mut person_child_count = None;
        let mut person_overview = None;
        let mut bio_info = None;

        let remote_result = {
            if let Some(session) = session.as_ref() {
                match adult_hub_client::AdultHubClient::new(&session.server_url) {
                    Ok(client) => {
                        let client = client.with_token(&session.access_token);
                        match client.get_performer_detail(&p_key) {
                            Ok(detail) => {
                                person_overview = detail.bio.clone();
                                person_child_count = detail.video_count.map(|c| c as u32);
                                bio_info = Some(adult_hub_client::PerformerBioDto {
                                    name: detail.name,
                                    bio: detail.bio,
                                    birth: detail.birth,
                                    nationality: detail.nationality,
                                    aliases: detail.aliases,
                                    photo_url: detail.photo_url.clone(),
                                    updated_at: None,
                                });
                                if let Some(photo_url) = detail.photo_url.as_deref() {
                                    if let Ok(resolved) = client.resolve_artwork_url(photo_url) {
                                        person_artwork =
                                            crate::thumbnails::fetch_and_cache_artwork(
                                                &resolved,
                                                Some(&session.access_token),
                                                crate::thumbnails::DETAIL_ARTWORK_WIDTH,
                                            )
                                            .ok()
                                            .map(|artwork| (resolved, artwork));
                                    }
                                }
                            }
                            Err(e) => {
                                crate::diagnostics::GLOBAL_LOG.w(
                                    "PerformerProfile",
                                    &format!("Failed to load performer detail for {p_key}: {e}"),
                                );
                            }
                        }
                        fetch_adult_hub_profile_items(&client, "performer", &p_key)
                    }
                    Err(e) => Err(e.to_string()),
                }
            } else {
                Ok(Vec::new())
            }
        };

        let is_remote_err = remote_result.is_err();
        let remote_err_msg = remote_result.as_ref().err().cloned();
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

        // 5. Update UI on event loop
        let p_key_clone = p_key.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            let current_id = ui.get_performer_id();
            let current_name = ui.get_performer_name();
            let matches_identity = if !current_id.is_empty() {
                current_id == p_key_clone.as_str()
            } else {
                current_name == p_key_clone.as_str()
            };
            if !matches_identity {
                return;
            }

            *performer_catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = filtered_items.clone();

            ui.set_performer_loading(false);

            // Update photo
            if let Some((url, artwork)) = person_artwork.as_ref() {
                let image = crate::thumbnails::remember_artwork_image(
                    url,
                    crate::thumbnails::DETAIL_ARTWORK_WIDTH,
                    artwork,
                );
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

            if let Some(bio) = bio_info.as_ref() {
                if !bio.name.is_empty() {
                    ui.set_performer_name(bio.name.clone().into());
                }
                if let Some(nat) = bio.nationality.as_deref() {
                    ui.set_performer_nationality(nat.into());
                }
                if let Some(birth) = bio.birth.as_deref() {
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
                    fetch_cards_thumbnails(weak.clone(), "performer", p_key_clone, cards, token);
                }
            }
        });
    });
}

fn load_studio_profile_data(ui: &MainWindow, ctx: &AppContext, s_name: String) {
    let weak = ui.as_weak();
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let cached_catalog = ctx.catalog.cached_catalog.clone();
    let home_catalog = ctx.catalog.home_catalog.clone();
    let studio_catalog = ctx.catalog.studio_catalog.clone();

    // 1. Find matching local items
    let mut local_items = Vec::new();
    {
        let cached = cached_catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = home_catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let remote_result = {
            if let Some(session) = session.as_ref() {
                match adult_hub_client::AdultHubClient::new(&session.server_url) {
                    Ok(client) => {
                        let client = client.with_token(&session.access_token);
                        fetch_adult_hub_profile_items(&client, "studio", &s_name)
                    }
                    Err(e) => Err(e.to_string()),
                }
            } else {
                Ok(Vec::new())
            }
        };

        let is_remote_err = remote_result.is_err();
        let remote_err_msg = remote_result.as_ref().err().cloned();
        let remote_items = remote_result.unwrap_or_default();

        let mut merged = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for item in remote_items.into_iter().chain(local_items) {
            if seen.insert(item.id.clone()) {
                merged.push(item);
            }
        }
        let filtered_items = filter_catalog_items(merged, &local_store);
        *studio_catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = filtered_items.clone();

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
        ui.on_open_performer_profile(move |id_or_name| {
            let Some(ui) = weak.upgrade() else { return };
            ui.invoke_cache_catalog_view();
            ctx_prof.push_nav_destination(crate::handlers::browsing::current_nav_destination(
                &ui, &ctx_prof,
            ));
            let key_str = id_or_name.to_string();
            let display_name = if key_str.starts_with("name:") {
                key_str.trim_start_matches("name:").trim().to_string()
            } else {
                key_str.clone()
            };
            ui.set_performer_id(key_str.clone().into());
            ui.set_performer_name(display_name.into());
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

            load_performer_profile_data(&ui, &ctx_prof, key_str);
        });
    }

    // 2. Retry Performer Profile
    {
        let weak = ui.as_weak();
        let ctx_retry = ctx.clone();
        ui.on_retry_performer_profile(move || {
            let Some(ui) = weak.upgrade() else { return };
            let id_str = ui.get_performer_id().to_string();
            let key_str = if !id_str.is_empty() {
                id_str
            } else {
                let name = ui.get_performer_name().to_string();
                if name.starts_with("name:") {
                    name
                } else if !name.is_empty() {
                    format!("name:{name}")
                } else {
                    String::new()
                }
            };
            if key_str.is_empty() {
                return;
            }
            ui.set_performer_loading(true);
            ui.set_performer_error("".into());
            load_performer_profile_data(&ui, &ctx_retry, key_str);
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
            ctx_prof.push_nav_destination(crate::handlers::browsing::current_nav_destination(
                &ui, &ctx_prof,
            ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn adult_hub_profile_items_page_through_catalog_videos() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut served = 0;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while let Ok(count) = stream.read(&mut buffer) {
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request).into_owned();
                let offset = request
                    .split("offset=")
                    .nth(1)
                    .and_then(|tail| tail.split('&').next())
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);

                let (count, has_more) = if offset == 0 {
                    (100, true)
                } else {
                    (25, false)
                };
                let items_json = (offset..offset + count)
                    .map(|i| format!(r#"{{"id":"vid-{i}","title":"Video {i}","duration_seconds":60,"performers":[],"categories":[],"tags":[]}}"#))
                    .collect::<Vec<_>>()
                    .join(",");
                let body = format!(
                    r#"{{"items":[{items_json}],"total_count":125,"offset":{offset},"limit":100,"has_more":{has_more}}}"#
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
                served += 1;
                if served >= 2 {
                    break;
                }
            }
        });

        let client = adult_hub_client::AdultHubClient::new(&format!("http://{address}"))
            .unwrap()
            .with_token("test-token");
        let items = fetch_adult_hub_profile_items(&client, "performer", "Performer A")
            .expect("adult hub paged query succeeds");

        assert_eq!(items.len(), 125, "all 125 items collected across 2 pages");
        assert_eq!(items[0].id, "vid-0");
        assert_eq!(items[124].id, "vid-124");
        let _ = server.join();
    }
}
