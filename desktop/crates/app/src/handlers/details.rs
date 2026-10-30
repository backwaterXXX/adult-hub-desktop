use crate::catalog::{
    filter_catalog_items, sample_catalog_tv_items, set_catalog, tv_item_to_card,
    tv_item_to_card_for_server,
};
use crate::context::AppContext;
use crate::playback::{format_time, show_error};
use crate::session::PlaybackSessionController;
use crate::{MainWindow, VideoCardData};
use adult_hub_client::{JellyfinClient, TvItem};
use slint::{ComponentHandle, Model};
use std::rc::Rc;

pub fn install_stage6_handlers(ui: &MainWindow, ctx: &AppContext) {
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let jellyfin = ctx.jellyfin.clone();
    let session_ctrl = ctx.session_ctrl.clone();
    let current_item = ctx.current_playing_item.clone();
    let cached_catalog = ctx.cached_catalog.clone();
    let home_catalog = ctx.home_catalog.clone();

    {
        let weak = ui.as_weak();
        ui.on_open_detail_source_url(move |raw_url| {
            let Some(ui) = weak.upgrade() else { return };
            let raw_url = raw_url.to_string();
            let Ok(parsed) = url::Url::parse(&raw_url) else {
                ui.set_status("Source URL is invalid".into());
                return;
            };
            if !matches!(parsed.scheme(), "http" | "https") {
                ui.set_status("Only HTTP and HTTPS source links can be opened".into());
                return;
            }
            if let Err(error) = webbrowser::open(parsed.as_str()) {
                eprintln!("Could not open source URL: {error}");
                ui.set_status("Could not open source URL".into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_copy_detail_item_id(move |item_id| {
            let Some(ui) = weak.upgrade() else { return };
            match crate::copy_text_to_clipboard(item_id.as_str()) {
                Ok(()) => ui.set_detail_id_copied(true),
                Err(error) => {
                    eprintln!("Could not copy item ID: {error}");
                    ui.set_detail_id_copied(false);
                    ui.set_status("Could not copy item ID to clipboard".into());
                }
            }
        });
    }

    // 1. Open Item Detail Modal
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let local_store = local_store.clone();
        let session_ctrl = session_ctrl.clone();
        let credential_store = credential_store.clone();
        let jellyfin = jellyfin.clone();
        let ctx_detail = ctx.clone();
        ui.on_open_item_detail(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let id = id.to_string();
            if ui.get_active_screen() == "browse" {
                ui.invoke_cache_catalog_view();
            }
            // Snapshot the screen being left (player, list, …) before any part
            // of this handler mutates UI state, so Back really returns there.
            ctx_detail.push_nav_destination(crate::handlers::browsing::current_nav_destination(
                &ui,
                &ctx_detail,
            ));
            let target_item = ctx_detail.find_catalog_item(&id).or_else(|| {
                if id == "sample" {
                    Some(sample_catalog_tv_items()[0].clone())
                } else {
                    None
                }
            });
            let target_item = match target_item {
                Some(item) => {
                    *ctx_detail.detail_active_item.lock().unwrap() = Some(item.clone());
                    item
                }
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
            let mut overview_str = target_item.overview.clone().unwrap_or_default();
            let mut extracted_channel = String::new();
            let mut extracted_source_url = String::new();

            // Clean up overview by extracting raw source/channel metadata and removing title repetitions
            let clean_title = target_item.title.trim();
            let disp_title_str = crate::catalog::display_title(&target_item);
            let disp_title = disp_title_str.trim();

            let mut clean_overview_lines = Vec::new();
            for raw_line in overview_str.lines() {
                let line = raw_line.trim();
                if line.is_empty() {
                    continue;
                }
                let lower = line.to_lowercase();
                if lower.starts_with("kanał:")
                    || lower.starts_with("kanal:")
                    || lower.starts_with("channel:")
                {
                    if let Some((_, val)) = line.split_once(':') {
                        let trimmed = val.trim();
                        if !trimmed.is_empty() && extracted_channel.is_empty() {
                            extracted_channel = trimmed.to_string();
                        }
                    }
                    continue;
                }
                if lower.starts_with("video page:")
                    || lower.starts_with("video url:")
                    || lower.starts_with("strona wideo:")
                    || lower.starts_with("url:")
                    || lower.starts_with("source:")
                {
                    if let Some((_, val)) = line.split_once(':') {
                        let trimmed = val.trim();
                        if !trimmed.is_empty() && extracted_source_url.is_empty() {
                            extracted_source_url = trimmed.to_string();
                        }
                    }
                    continue;
                }
                if lower.starts_with("tytuł:") || lower.starts_with("title:") {
                    continue;
                }
                if line.eq_ignore_ascii_case(clean_title) || line.eq_ignore_ascii_case(disp_title) {
                    continue;
                }
                clean_overview_lines.push(raw_line.trim_end());
            }
            overview_str = clean_overview_lines.join("\n").trim().to_string();

            let original_title_str = if clean_title.eq_ignore_ascii_case(disp_title) {
                String::new()
            } else {
                target_item.title.clone()
            };

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
            let backdrop_url = if !is_telegram_item {
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
            } else {
                // Telegram pHash images are poster frames, not wide backdrops.
                // The detail page renders the poster in its contain-fit header.
                None
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
            let resume_ms = PlaybackSessionController::resolve_item_resume_position(
                &local_store,
                &session_ctrl,
                &target_item,
            );

            let can_resume = resume_ms
                .is_some_and(|ms| PlaybackSessionController::should_prompt_resume(ms, duration_ms));
            let resume_text = if can_resume {
                format_time(resume_ms.unwrap_or(0) as f64 / 1000.0)
            } else {
                String::new()
            };

            let genre_lower: std::collections::HashSet<String> = target_item
                .genres
                .iter()
                .chain(target_item.tags.iter())
                .map(|g| g.trim().to_lowercase())
                .collect();

            let is_category_word = |name: &str| -> bool {
                let lower = name.trim().to_lowercase();
                if lower.is_empty() || genre_lower.contains(&lower) {
                    return true;
                }
                const KNOWN_CATEGORIES: &[&str] = &[
                    "locker room",
                    "body worship",
                    "behind the scenes",
                    "compilation",
                    "casting",
                    "threesome",
                    "foursome",
                    "solo",
                    "interracial",
                    "outdoor",
                    "public",
                    "pov",
                    "point of view",
                    "parody",
                    "fetish",
                    "foot fetish",
                    "massage",
                    "oil massage",
                    "deepthroat",
                    "blowjob",
                    "creampie",
                    "facial",
                    "masturbation",
                    "striptease",
                    "bdsm",
                    "bondage",
                    "roleplay",
                    "cosplay",
                    "hardcore",
                    "softcore",
                    "amateur",
                    "vintage",
                    "retro",
                    "uniform",
                    "lingerie",
                    "stockings",
                    "anal",
                    "oral",
                    "glamour",
                    "erotic",
                ];
                KNOWN_CATEGORIES.iter().any(|cat| lower == *cat)
            };

            let performers = target_item
                .people
                .iter()
                .filter(|p| !is_category_word(&p.name))
                .map(|p| p.name.clone().into())
                .collect::<Vec<slint::SharedString>>();
            let genre_items = target_item
                .genres
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<slint::SharedString>>();

            let mut performer_image_jobs = Vec::new();
            let person_width = crate::thumbnails::person_thumbnail_max_width();
            let performer_cards = target_item
                .people
                .iter()
                .filter(|person| {
                    if is_category_word(&person.name) {
                        return false;
                    }
                    person
                        .person_type
                        .as_deref()
                        .is_none_or(|kind| kind.eq_ignore_ascii_case("actor"))
                })
                .take(20)
                .map(|person| {
                    // A stable Jellyfin person GUID plus its Primary tag yields
                    // the exact URL the profile screen uses for this actor, so
                    // both surfaces share one portrait cache entry. Name-slug
                    // IDs (resolver items) and tag-less entries are resolved
                    // through the Persons endpoint in the worker below.
                    let direct_url = artwork_server.as_ref().and_then(|server| {
                        let tag = person
                            .primary_image_tag
                            .as_deref()
                            .filter(|tag| !tag.trim().is_empty())?;
                        if uuid::Uuid::parse_str(&person.id).is_err() {
                            return None;
                        }
                        Some(JellyfinClient::image_url(
                            server,
                            &person.id,
                            "Primary",
                            person_width,
                            Some(tag),
                        ))
                    });
                    let cached_image = direct_url
                        .as_deref()
                        .and_then(crate::thumbnails::load_cached_image);
                    if cached_image.is_none() && artwork_server.is_some() {
                        performer_image_jobs.push((
                            person.id.clone(),
                            direct_url,
                            person.name.clone(),
                        ));
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

            let mut combined_tags: Vec<String> = Vec::new();
            let mut add_tag = |raw_tag: &str| {
                let cleaned = raw_tag.trim().trim_start_matches('#').trim();
                if !cleaned.is_empty()
                    && !combined_tags
                        .iter()
                        .any(|existing: &String| existing.eq_ignore_ascii_case(cleaned))
                {
                    combined_tags.push(cleaned.to_string());
                }
            };

            for t in &target_item.tags {
                add_tag(t);
            }
            let (_, extracted_tags) =
                crate::catalog::extract_title_tags_and_clean_title(&target_item.title);
            for t in extracted_tags {
                add_tag(&t);
            }
            if let Some(overview) = target_item.overview.as_deref() {
                let (_, overview_tags) =
                    crate::catalog::extract_title_tags_and_clean_title(overview);
                for t in overview_tags {
                    add_tag(&t);
                }
            }
            let tag_items = combined_tags
                .into_iter()
                .map(Into::into)
                .collect::<Vec<slint::SharedString>>();

            let size_str = target_item.format_media_size();

            let provider_str = target_item.source_provider.clone().unwrap_or_else(|| {
                if is_telegram_item {
                    "Telegram".to_string()
                } else {
                    "Jellyfin".to_string()
                }
            });

            ui.set_detail_id(target_item.id.clone().into());
            ui.set_detail_title(crate::catalog::display_title(&target_item).into());
            ui.set_detail_original_title(original_title_str.into());
            ui.set_detail_channel(extracted_channel.into());
            ui.set_detail_source_url(extracted_source_url.into());
            ui.set_detail_id_copied(false);
            ui.set_detail_overview(overview_str.into());
            ui.set_detail_year(year_str.into());
            ui.set_detail_duration(duration_str.into());
            ui.set_detail_studio(studio_str.into());
            ui.set_detail_genres(genres_str.into());
            ui.set_detail_genre_items(Rc::new(slint::VecModel::from(genre_items)).into());
            ui.set_detail_tag_items(Rc::new(slint::VecModel::from(tag_items)).into());
            ui.set_detail_resolution(resolution_str.into());
            ui.set_detail_container(target_item.container.clone().unwrap_or_default().into());
            ui.set_detail_file_size(size_str.into());
            ui.set_detail_provider(provider_str.into());
            ui.set_detail_tech_expanded(false);
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

            // The library entry size is unreliable for referrals (.strm) and is
            // often absent entirely. Resolve the real media size off the UI
            // thread from trusted sources, keeping "Unavailable" when unknown.
            if target_item.media_size_bytes().is_none() {
                let weak = weak.clone();
                let item_id = target_item.id.clone();
                let item_for_size = target_item.clone();
                let session_for_size = artwork_session.clone();
                let jellyfin_for_size = jellyfin.clone();
                let ctx_for_size = ctx_detail.clone();
                std::thread::spawn(move || {
                    let Some(bytes) = resolve_real_media_size(
                        &item_for_size,
                        &ctx_for_size,
                        session_for_size.as_ref(),
                        &jellyfin_for_size,
                    ) else {
                        return;
                    };
                    let text = adult_hub_player::format_file_size(bytes);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade()
                            && ui.get_detail_id() == item_id.as_str()
                        {
                            ui.set_detail_file_size(text.into());
                        }
                    });
                });
            }

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
                .map(|(_, item)| {
                    if let Some(session) = &artwork_session {
                        tv_item_to_card_for_server(&item, &session.server_url)
                    } else {
                        tv_item_to_card(&item)
                    }
                })
                .collect();
            let initial_sim_image_jobs: Vec<(String, crate::catalog::ThumbnailRequest)> =
                similar_cards
                    .iter()
                    .filter(|card| {
                        !card.has_thumbnail
                            || (card.provider == "telegram"
                                && !card.fallback_thumbnail_url.is_empty())
                    })
                    .map(|card| {
                        (
                            card.id.to_string(),
                            crate::catalog::ThumbnailRequest::from(card),
                        )
                    })
                    .collect();
            let sim_model = Rc::new(slint::VecModel::from(similar_cards));
            ui.set_detail_similar_items(sim_model.into());
            ui.set_detail_similar_loading(!is_telegram_item && artwork_session.is_some());

            if !initial_sim_image_jobs.is_empty() {
                let weak = weak.clone();
                let detail_id = target_item.id.clone();
                let token = artwork_token.clone();
                std::thread::spawn(move || {
                    for (card_id, request) in initial_sim_image_jobs {
                        let Some(thumbnail) =
                            crate::catalog::fetch_card_thumbnail(&request, token.as_deref())
                        else {
                            continue;
                        };
                        let weak = weak.clone();
                        let detail_id = detail_id.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade()
                                && ui.get_detail_id() == detail_id.as_str()
                                && let Some(image) =
                                    crate::thumbnails::image_from_bytes(&thumbnail.bytes)
                            {
                                let model = ui.get_detail_similar_items();
                                let mut cards = (0..model.row_count())
                                    .filter_map(|row| model.row_data(row))
                                    .collect::<Vec<_>>();
                                // Attach by stable record id, never by the index
                                // captured when the batch started.
                                if let Some(card) =
                                    cards.iter_mut().find(|card| card.id == card_id.as_str())
                                {
                                    card.thumbnail = image;
                                    card.thumbnail_url = thumbnail.url.into();
                                    card.has_thumbnail = true;
                                    card.thumbnail_state = "ready".into();
                                    ui.set_detail_similar_items(
                                        Rc::new(slint::VecModel::from(cards)).into(),
                                    );
                                }
                            }
                        });
                    }
                });
            }

            ui.set_detail_visible(false);
            ui.set_active_screen("video-details".into());

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
                let session_for_cast = artwork_session.clone();
                let jellyfin_for_cast = jellyfin.clone();
                std::thread::spawn(move || {
                    let Some(session) = session_for_cast else {
                        return;
                    };
                    let token = session.access_token.as_str();
                    for (person_id, direct_url, name) in performer_image_jobs {
                        let mut bytes = direct_url.as_deref().and_then(|url| {
                            crate::thumbnails::load_cached_image_bytes(url).or_else(|| {
                                crate::thumbnails::fetch_and_cache_image_bytes(url, Some(token))
                            })
                        });
                        if bytes.is_none() {
                            // Fall back to the actor data service: the Persons
                            // endpoint returns the stable GUID and Primary tag,
                            // so the retrieved portrait equals the one shown on
                            // the actor profile and in the Performers shelf.
                            if let Ok(person) = jellyfin_for_cast.get_person(
                                &session.server_url,
                                &session.user_id,
                                token,
                                &name,
                            ) && let Some(tag) = person
                                .primary_image_tag
                                .as_deref()
                                .filter(|tag| !tag.trim().is_empty())
                            {
                                let url = JellyfinClient::image_url(
                                    &session.server_url,
                                    &person.id,
                                    "Primary",
                                    person_width,
                                    Some(tag),
                                );
                                bytes = crate::thumbnails::load_cached_image_bytes(&url).or_else(
                                    || {
                                        crate::thumbnails::fetch_and_cache_image_bytes(
                                            &url,
                                            Some(token),
                                        )
                                    },
                                );
                            }
                        }
                        let Some(image_bytes) = bytes else {
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
                                // Attach by stable person id, never by the index
                                // captured when the batch started.
                                if let Some(card) =
                                    cards.iter_mut().find(|card| card.id == person_id.as_str())
                                {
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
                                if !similar_items.is_empty() {
                                    let cards = similar_items
                                        .iter()
                                        .map(|item| tv_item_to_card_for_server(item, &server_url))
                                        .collect::<Vec<_>>();
                                    let image_jobs = cards
                                        .iter()
                                        .filter(|card| {
                                            !card.has_thumbnail
                                                || (card.provider == "telegram"
                                                    && !card.fallback_thumbnail_url.is_empty())
                                        })
                                        .map(|card| {
                                            (
                                                card.id.to_string(),
                                                crate::catalog::ThumbnailRequest::from(card),
                                            )
                                        })
                                        .collect::<Vec<_>>();
                                    ui.set_detail_similar_items(
                                        Rc::new(slint::VecModel::from(cards)).into(),
                                    );

                                    if !image_jobs.is_empty() {
                                        let weak = weak.clone();
                                        let detail_id = detail_id_for_ui.clone();
                                        std::thread::spawn(move || {
                                            for (card_id, request) in image_jobs {
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
                                                        // Attach by stable record id, never by
                                                        // the index captured at batch start.
                                                        if let Some(card) =
                                                            cards.iter_mut().find(|card| {
                                                                card.id == card_id.as_str()
                                                            })
                                                        {
                                                            card.thumbnail = image;
                                                            card.thumbnail_url =
                                                                thumbnail.url.into();
                                                            card.has_thumbnail = true;
                                                            card.thumbnail_state = "ready".into();
                                                            ui.set_detail_similar_items(
                                                                Rc::new(slint::VecModel::from(
                                                                    cards,
                                                                ))
                                                                .into(),
                                                            );
                                                        }
                                                    }
                                                });
                                            }
                                        });
                                    }
                                }
                                ui.set_detail_similar_loading(false);
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
        let player = ctx.player.clone();
        let ctx_close = ctx.clone();
        ui.on_close_item_detail(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_detail_visible(false);
                if ui.get_active_screen() == "video-details" {
                    ui.invoke_navigate_back();
                    return;
                }
                if ui.get_active_screen() == "player" {
                    let was_playing = ctx_close
                        .player_paused_for_details
                        .swap(false, std::sync::atomic::Ordering::SeqCst);
                    if was_playing {
                        if let Some(p) = &player {
                            let _ = p.set_pause(false);
                        }
                        ui.set_playing(true);
                    }
                }
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

            // Only marking an item as watched clears the saved position.
            // Un-marking must not discard resume progress.
            if new_played {
                let _ = local_store.save_progress("local", &id, 0, 0);
            }
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
        let ctx_play = ctx.clone();
        let player = ctx.player.clone();
        let cached_catalog = cached_catalog.clone();
        ui.on_play_detail_item(move |id, resume| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_detail_visible(false);
            let id = id.to_string();

            if ui.get_active_screen() == "player" {
                let current_id = ctx_play
                    .current_playing_item
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|i| i.id.clone());
                if current_id.as_deref() == Some(&id) {
                    ctx_play
                        .player_paused_for_details
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    if resume {
                        // Keep current media and position without re-resolving or resetting player
                        if let Some(player) = &player {
                            let _ = player.set_pause(false);
                        }
                        ui.set_playing(true);
                        return;
                    } else {
                        // Play from start: seek to 0 without re-resolving
                        if let Some(player) = &player {
                            let _ = player.seek_absolute(0.0);
                            let _ = player.set_pause(false);
                        }
                        ui.set_playing(true);
                        return;
                    }
                }
            }

            let target_item = ctx_play
                .find_catalog_item(&id)
                .or_else(|| ctx_play.detail_active_item.lock().unwrap().clone())
                .or_else(|| {
                    if id == "sample" {
                        Some(sample_catalog_tv_items()[0].clone())
                    } else {
                        None
                    }
                });
            let target_item = match target_item {
                Some(item) => item,
                None => {
                    show_error(
                        &ui,
                        "This item is no longer in the current catalog. Refresh and try again.",
                    );
                    return;
                }
            };

            ctx_play.set_playback_context(cached_catalog.lock().unwrap().clone());
            crate::playback::start_item_playback_with_resume_ctx(
                &ui,
                &ctx_play,
                target_item,
                Some(resume),
            );
        });
    }

    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let ctx_random = ctx.clone();
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
            ctx_random.set_playback_context(catalog.clone());
            crate::playback::start_item_playback_ctx(&ui, &ctx_random, target);
        });
    }

    // 6. Select Performer (Direct to Performer Profile Screen)
    {
        let weak = ui.as_weak();
        ui.on_select_performer(move |performer| {
            let Some(ui) = weak.upgrade() else { return };
            ui.invoke_open_performer_profile(performer);
        });
    }

    // 7. Select Studio (Direct to Studio Details Screen)
    {
        let weak = ui.as_weak();
        ui.on_select_studio(move |studio| {
            let Some(ui) = weak.upgrade() else { return };
            ui.invoke_open_studio_profile(studio);
        });
    }
}

/// Sizes below this are error documents, playlists or `.strm` referral bodies,
/// never the media itself. Same threshold the Details formatter uses.
const MIN_PLAUSIBLE_MEDIA_BYTES: u64 = adult_hub_client::TvItem::MIN_PLAUSIBLE_MEDIA_BYTES;

fn content_range_total_bytes(value: &str) -> Option<u64> {
    let total = value.rsplit('/').next()?.trim();
    if total == "*" {
        return None;
    }
    total.parse::<u64>().ok()
}

fn is_non_media_content_type(content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("xml")
        || ct.contains("mpegurl")
        || ct.contains("m3u")
}

/// Ask a stream URL for its total size with a `bytes=0-0` range probe. Only
/// the origin that issued the URL receives credentials; redirects are
/// followed manually without auth headers. Also used by the connection
/// diagnostics as a stream-access check.
pub(crate) fn probe_remote_media_size(
    url: &str,
    token: Option<&str>,
    extra_headers: &[String],
) -> Option<u64> {
    let original = url::Url::parse(url).ok()?;
    if !matches!(original.scheme(), "http" | "https") {
        return None;
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;
    let mut current = original.clone();
    for _ in 0..=3 {
        let mut request = client
            .get(current.clone())
            .header(reqwest::header::RANGE, "bytes=0-0");
        for header in extra_headers {
            if let Some((name, value)) = header.split_once(':') {
                request = request.header(name.trim(), value.trim());
            }
        }
        if current.origin() == original.origin()
            && let Some(token) = token.filter(|token| !token.is_empty())
        {
            request = request.header("X-Emby-Token", token);
        }
        let Ok(response) = request.send() else {
            return None;
        };
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)?;
            let Ok(next) = response.url().join(&location) else {
                return None;
            };
            if !matches!(next.scheme(), "http" | "https") {
                return None;
            }
            current = next;
            continue;
        }
        if !response.status().is_success() {
            return None;
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        if is_non_media_content_type(&content_type) {
            return None;
        }
        if let Some(total) = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(content_range_total_bytes)
            && total >= MIN_PLAUSIBLE_MEDIA_BYTES
        {
            return Some(total);
        }
        if let Some(len) = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            && len >= MIN_PLAUSIBLE_MEDIA_BYTES
        {
            return Some(len);
        }
        return None;
    }
    None
}

/// Trusted media size, tried in order: completed download, the active player
/// stream, Jellyfin's resolved playback URL, then the resolver stream URL.
fn resolve_real_media_size(
    item: &TvItem,
    ctx: &AppContext,
    session: Option<&adult_hub_storage::Session>,
    jellyfin: &JellyfinClient,
) -> Option<u64> {
    if let Some(done) = ctx
        .download_manager
        .history_items()
        .into_iter()
        .find(|entry| entry.item_id == item.id)
    {
        if done.file_size_bytes >= MIN_PLAUSIBLE_MEDIA_BYTES {
            return Some(done.file_size_bytes);
        }
        if let Ok(meta) = std::fs::metadata(&done.file_path)
            && meta.len() >= MIN_PLAUSIBLE_MEDIA_BYTES
        {
            return Some(meta.len());
        }
    }

    let playing_matches = ctx
        .current_playing_item
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|playing| playing.id == item.id);
    if playing_matches
        && let Some(player) = &ctx.player
        && let Some(size) = player.get_u64("file-size")
        && size >= MIN_PLAUSIBLE_MEDIA_BYTES
    {
        return Some(size);
    }

    let session = session?;
    if uuid::Uuid::parse_str(&item.id).is_ok()
        && let Ok(resolution) = jellyfin.resolve_playback(
            &session.server_url,
            &session.user_id,
            &session.access_token,
            &item.id,
        )
        && let Some(size) = probe_remote_media_size(
            &resolution.stream_url,
            Some(&session.access_token),
            &resolution.required_headers,
        )
    {
        return Some(size);
    }

    if item.is_telegram() {
        let resolver_url =
            AppContext::resolve_effective_resolver_url(&ctx.local_store, &ctx.credential_store);
        if let Ok((stream_url, _method)) = PlaybackSessionController::resolve_stream_url(
            item,
            Some(&session.server_url),
            resolver_url.as_deref(),
            Some(&session.access_token),
        ) && let Some(size) = probe_remote_media_size(&stream_url, None, &[])
        {
            return Some(size);
        }
    }

    None
}
