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

fn fetch_profile_thumbnails(
    weak: slint::Weak<MainWindow>,
    performer_name: String,
    cards: Vec<crate::VideoCardData>,
    token: String,
) {
    let jobs = cards
        .iter()
        .enumerate()
        .filter(|(_, card)| {
            !card.has_thumbnail
                || (card.provider == "telegram" && !card.fallback_thumbnail_url.is_empty())
        })
        .map(|(index, card)| (index, crate::catalog::ThumbnailRequest::from(card)))
        .collect::<Vec<_>>();
    if jobs.is_empty() {
        return;
    }

    std::thread::spawn(move || {
        for (index, card) in jobs {
            let Some(thumbnail) = crate::catalog::fetch_card_thumbnail(&card, Some(&token)) else {
                continue;
            };
            let weak = weak.clone();
            let performer_name = performer_name.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade()
                    && ui.get_performer_name() == performer_name.as_str()
                    && let Some(image) = crate::thumbnails::image_from_bytes(&thumbnail.bytes)
                {
                    let model = ui.get_performer_items();
                    let mut items = (0..model.row_count())
                        .filter_map(|row| model.row_data(row))
                        .collect::<Vec<_>>();
                    if let Some(item) = items.get_mut(index) {
                        item.thumbnail = image;
                        item.thumbnail_url = thumbnail.url.into();
                        item.has_thumbnail = true;
                        ui.set_performer_items(Rc::new(slint::VecModel::from(items)).into());
                    }
                }
            });
        }
    });
}

pub fn install_stage7_handlers(ui: &MainWindow, ctx: &AppContext) {
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let jellyfin = ctx.jellyfin.clone();
    let resolver = ctx.resolver.clone();
    let cached_catalog = ctx.cached_catalog.clone();

    // 1. Open Performer Profile Dialog
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let resolver = resolver.clone();

        ui.on_open_performer_profile(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            let name_str = name.to_string();
            ui.set_performer_name(name.clone());
            ui.set_performer_movie_count(0);
            ui.set_performer_has_image(false);
            ui.set_performer_image(slint::Image::default());
            ui.set_performer_bio("".into());
            ui.set_performer_nationality("".into());
            ui.set_performer_birth("".into());
            ui.set_performer_aliases("".into());

            // Initial catalog match for filmography
            let matching: Vec<TvItem> = cached_catalog
                .lock()
                .unwrap()
                .iter()
                .filter(|i| {
                    i.people
                        .iter()
                        .any(|p| p.name.eq_ignore_ascii_case(&name_str))
                })
                .cloned()
                .collect();

            let person = cached_catalog
                .lock()
                .unwrap()
                .iter()
                .flat_map(|item| item.people.iter())
                .find(|person| person.name.eq_ignore_ascii_case(&name_str))
                .cloned();
            if let (Some(person), Ok(Some(session))) = (person, credential_store.get()) {
                let image_url = JellyfinClient::image_url(
                    &session.server_url,
                    &person.id,
                    "Primary",
                    crate::thumbnails::person_thumbnail_max_width(),
                    person.primary_image_tag.as_deref(),
                );
                let cached_image = crate::thumbnails::load_cached_image(&image_url);
                if let Some(image) = cached_image {
                    ui.set_performer_image(image);
                    ui.set_performer_has_image(true);
                } else {
                    let weak = weak.clone();
                    let performer_name = name_str.clone();
                    let token = session.access_token.clone();
                    std::thread::spawn(move || {
                        let Some(image_bytes) = crate::thumbnails::fetch_and_cache_image_bytes(
                            &image_url,
                            Some(&token),
                        ) else {
                            return;
                        };
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade()
                                && ui.get_performer_name() == performer_name.as_str()
                                && let Some(image) =
                                    crate::thumbnails::image_from_bytes(&image_bytes)
                            {
                                ui.set_performer_image(image);
                                ui.set_performer_has_image(true);
                            }
                        });
                    });
                }
            }

            let session = credential_store.get().ok().flatten();
            let cards = profile_cards(
                &matching,
                session.as_ref().map(|session| session.server_url.as_str()),
            );
            let model = Rc::new(slint::VecModel::from(cards));
            ui.set_performer_items(model.into());
            ui.set_performer_profile_visible(true);

            if let Some(session) = session {
                let cards = (0..ui.get_performer_items().row_count())
                    .filter_map(|row| ui.get_performer_items().row_data(row))
                    .collect();
                fetch_profile_thumbnails(
                    weak.clone(),
                    name_str.clone(),
                    cards,
                    session.access_token,
                );
            }

            // Fetch performer bio & remote filmography asynchronously
            let weak = weak.clone();
            let local_store = local_store.clone();
            let credential_store = credential_store.clone();
            let jellyfin = jellyfin.clone();
            let resolver = resolver.clone();
            let p_name = name_str.clone();

            std::thread::spawn(move || {
                let session = credential_store.get().ok().flatten();
                if let Some(session) = session.as_ref() {
                    let person_query = GetItemsQuery {
                        search_term: Some(&p_name),
                        sort_by: "SortName",
                        limit: 20,
                        include_item_types: Some("Person"),
                        person_types: Some("Actor"),
                        ..Default::default()
                    };
                    if let Ok(page) = jellyfin.get_items(
                        &session.server_url,
                        &session.user_id,
                        &session.access_token,
                        &person_query,
                    ) && let Some(person) = page
                        .items
                        .into_iter()
                        .find(|person| person.title.eq_ignore_ascii_case(&p_name))
                    {
                        let person_name = p_name.clone();
                        let weak_person = weak.clone();
                        let image_url = JellyfinClient::image_url(
                            &session.server_url,
                            &person.id,
                            "Primary",
                            crate::thumbnails::person_thumbnail_max_width(),
                            person.primary_image_tag.as_deref(),
                        );
                        let image_bytes = crate::thumbnails::load_cached_image_bytes(&image_url)
                            .or_else(|| {
                                crate::thumbnails::fetch_and_cache_image_bytes(
                                    &image_url,
                                    Some(&session.access_token),
                                )
                            });
                        let movie_count = person.child_count.unwrap_or(0) as i32;
                        let overview = person.overview.unwrap_or_default();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak_person.upgrade()
                                && ui.get_performer_name() == person_name.as_str()
                            {
                                ui.set_performer_movie_count(movie_count);
                                if ui.get_performer_bio().is_empty() && !overview.is_empty() {
                                    ui.set_performer_bio(overview.into());
                                }
                                if let Some(image) = image_bytes
                                    .as_deref()
                                    .and_then(crate::thumbnails::image_from_bytes)
                                {
                                    ui.set_performer_image(image);
                                    ui.set_performer_has_image(true);
                                }
                            }
                        });
                    }
                }

                let res_url = local_store
                    .get_setting("resolver_base_url")
                    .ok()
                    .flatten()
                    .or_else(|| credential_store.get().ok().flatten().map(|s| s.server_url))
                    .unwrap_or_default();

                if !res_url.is_empty() {
                    if let Ok(bio_dto) = resolver.get_performer_bio(&res_url, &p_name) {
                        let weak2 = weak.clone();
                        let profile_name_for_bio = p_name.clone();
                        slint::invoke_from_event_loop(move || {
                            let Some(ui) = weak2.upgrade() else { return };
                            if ui.get_performer_name() != profile_name_for_bio.as_str() {
                                return;
                            }
                            if let Some(bio) = bio_dto.bio {
                                ui.set_performer_bio(bio.into());
                            }
                            if let Some(nat) = bio_dto.nationality {
                                ui.set_performer_nationality(nat.into());
                            }
                            if let Some(birth) = bio_dto.birth {
                                ui.set_performer_birth(birth.into());
                            }
                            if !bio_dto.aliases.is_empty() {
                                ui.set_performer_aliases(bio_dto.aliases.join(", ").into());
                            }
                        })
                        .ok();
                    }
                }

                // If logged in to Jellyfin, fetch person filmography
                if let Ok(Some(sess)) = credential_store.get() {
                    let q = GetItemsQuery {
                        person: Some(&p_name),
                        sort_by: "SortName",
                        limit: 50,
                        ..Default::default()
                    };
                    if let Ok(page) =
                        jellyfin.get_items(&sess.server_url, &sess.user_id, &sess.access_token, &q)
                    {
                        let filtered = filter_catalog_items(page.items, &local_store);
                        let server_url = sess.server_url.clone();
                        let token = sess.access_token.clone();
                        let weak2 = weak.clone();
                        let image_weak = weak.clone();
                        let profile_name_for_movies = p_name.clone();
                        slint::invoke_from_event_loop(move || {
                            let Some(ui) = weak2.upgrade() else { return };
                            if ui.get_performer_name() != profile_name_for_movies.as_str() {
                                return;
                            }
                            let cards = profile_cards(&filtered, Some(&server_url));
                            let model = Rc::new(slint::VecModel::from(cards));
                            ui.set_performer_items(model.into());
                            let model = ui.get_performer_items();
                            let image_cards = (0..model.row_count())
                                .filter_map(|index| model.row_data(index))
                                .collect();
                            fetch_profile_thumbnails(
                                image_weak,
                                profile_name_for_movies,
                                image_cards,
                                token,
                            );
                        })
                        .ok();
                    }
                }
            });
        });
    }

    // 2. Close Performer Profile Dialog
    {
        let weak = ui.as_weak();
        ui.on_close_performer_profile(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_performer_profile_visible(false);
            }
        });
    }

    // 3. Open Studio Profile Dialog
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();

        ui.on_open_studio_profile(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            let studio_str = name.to_string();
            ui.set_studio_name(name.clone());

            let matching: Vec<TvItem> = cached_catalog
                .lock()
                .unwrap()
                .iter()
                .filter(|i| {
                    i.studios
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(&studio_str))
                })
                .cloned()
                .collect();
            let cards = matching.iter().map(tv_item_to_card).collect::<Vec<_>>();
            let model = Rc::new(slint::VecModel::from(cards));
            ui.set_studio_items(model.into());
            ui.set_studio_profile_visible(true);

            // Fetch studio releases from Jellyfin if connected
            let weak = weak.clone();
            let local_store = local_store.clone();
            let credential_store = credential_store.clone();
            let jellyfin = jellyfin.clone();
            let s_name = studio_str.clone();

            std::thread::spawn(move || {
                if let Ok(Some(sess)) = credential_store.get() {
                    let q = GetItemsQuery {
                        studio: Some(&s_name),
                        sort_by: "SortName",
                        limit: 50,
                        ..Default::default()
                    };
                    if let Ok(page) =
                        jellyfin.get_items(&sess.server_url, &sess.user_id, &sess.access_token, &q)
                    {
                        let filtered = filter_catalog_items(page.items, &local_store);
                        let weak2 = weak.clone();
                        slint::invoke_from_event_loop(move || {
                            let Some(ui) = weak2.upgrade() else { return };
                            let cards = filtered.iter().map(tv_item_to_card).collect::<Vec<_>>();
                            let model = Rc::new(slint::VecModel::from(cards));
                            ui.set_studio_items(model.into());
                        })
                        .ok();
                    }
                }
            });
        });
    }

    // 4. Close Studio Profile Dialog
    {
        let weak = ui.as_weak();
        ui.on_close_studio_profile(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_studio_profile_visible(false);
            }
        });
    }
}
