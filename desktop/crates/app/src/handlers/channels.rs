use crate::MainWindow;
use crate::catalog::{
    append_channel_page, filter_catalog_items, refresh_channel_thumbnails_async,
    set_channel_catalog, tv_item_to_card,
};
use crate::context::AppContext;
use adult_hub_client::{ResolverError, TvItem};
use slint::ComponentHandle;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

fn resolver_error_summary(error: &ResolverError) -> String {
    match error {
        ResolverError::Http(error) if error.is_timeout() => "request timed out".into(),
        ResolverError::Http(error) if error.is_connect() => "could not reach the Resolver".into(),
        ResolverError::Http(error) => error
            .status()
            .map(|status| format!("Resolver returned HTTP {}", status.as_u16()))
            .unwrap_or_else(|| "network request failed".into()),
        ResolverError::Serialization(_) => "Resolver returned an invalid response".into(),
        ResolverError::Auth(code) => format!("Resolver rejected authorization (HTTP {code})"),
        ResolverError::NotFound => "Resolver endpoint was not found (HTTP 404)".into(),
        ResolverError::Conflict => "Resolver could not complete the request (HTTP 409)".into(),
        ResolverError::Status(code, _) => format!("Resolver returned HTTP {code}"),
        ResolverError::Api(_) => "Resolver rejected the request".into(),
    }
}

fn resolver_error_kind(error: &ResolverError) -> &'static str {
    match error {
        ResolverError::Auth(403) | ResolverError::Status(403, _) => "access-denied",
        ResolverError::Http(error)
            if error.status().is_some_and(|status| status.as_u16() == 403) =>
        {
            "access-denied"
        }
        ResolverError::Http(error) if error.is_timeout() || error.is_connect() => "offline",
        _ => "error",
    }
}

fn filter_sort_channel_items(
    mut items: Vec<TvItem>,
    query: &str,
    duration_filter: u8,
    sort_mode: u8,
) -> Vec<TvItem> {
    let query = query.trim().to_lowercase();
    items.retain(|item| {
        let matches_query = query.is_empty()
            || item.title.to_lowercase().contains(&query)
            || item
                .overview
                .as_ref()
                .is_some_and(|overview| overview.to_lowercase().contains(&query))
            || item
                .studios
                .iter()
                .any(|studio| studio.to_lowercase().contains(&query))
            || item
                .people
                .iter()
                .any(|person| person.name.to_lowercase().contains(&query));
        if !matches_query {
            return false;
        }
        if duration_filter == 0 {
            return true;
        }
        let minutes = item.runtime_ticks.map(|ticks| ticks / 10_000_000 / 60);
        match (duration_filter, minutes) {
            (1, Some(1..=14)) => true,
            (2, Some(15..=40)) => true,
            (3, Some(minutes)) => minutes > 40,
            _ => false,
        }
    });
    match sort_mode {
        1 => items.sort_by(|a, b| {
            b.runtime_ticks
                .unwrap_or(0)
                .cmp(&a.runtime_ticks.unwrap_or(0))
                .then_with(|| b.id.cmp(&a.id))
        }),
        2 => items.sort_by(|a, b| {
            a.runtime_ticks
                .filter(|ticks| *ticks > 0)
                .unwrap_or(i64::MAX)
                .cmp(
                    &b.runtime_ticks
                        .filter(|ticks| *ticks > 0)
                        .unwrap_or(i64::MAX),
                )
                .then_with(|| a.id.cmp(&b.id))
        }),
        _ => {}
    }
    items
}

#[derive(Clone, Default)]
struct ChannelCacheEntry {
    items: Vec<TvItem>,
    next_offset: Option<i64>,
    has_more: bool,
}

pub fn install_stage8_handlers(ui: &MainWindow, ctx: &AppContext) {
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let resolver = ctx.resolver.clone();
    let cached_catalog = ctx.cached_catalog.clone();

    let current_channel_id = Arc::new(Mutex::new(String::new()));
    let channel_generation = Arc::new(AtomicU64::new(0));
    let category_ids = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let channel_offset_id = Arc::new(Mutex::new(None::<i64>));
    let channel_items_cache = Arc::new(Mutex::new(Vec::<TvItem>::new()));
    let channel_cache_map = Arc::new(Mutex::new(HashMap::<String, ChannelCacheEntry>::new()));
    let saved_sort_mode = match local_store
        .get_setting("channel_sort_mode")
        .ok()
        .flatten()
        .as_deref()
    {
        Some("longest") => 1,
        Some("shortest") => 2,
        _ => 0,
    };
    let channel_sort_mode = Arc::new(Mutex::new(saved_sort_mode));
    let channel_duration_filter = Arc::new(Mutex::new(0u8));
    ui.set_channel_sort_mode(saved_sort_mode as i32);
    ui.set_channel_duration_filter(0);

    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let resolver = resolver.clone();
        let category_ids = category_ids.clone();
        ui.on_refresh_channel_categories(move || {
            let Some(_ui) = weak.upgrade() else { return };
            let weak = weak.clone();
            let local_store = local_store.clone();
            let credential_store = credential_store.clone();
            let resolver = resolver.clone();
            let category_ids = category_ids.clone();
            std::thread::spawn(move || {
                let res_url = local_store
                    .get_setting("resolver_base_url")
                    .ok()
                    .flatten()
                    .or_else(|| credential_store.get().ok().flatten().map(|s| s.server_url))
                    .unwrap_or_default();
                if res_url.is_empty() {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_channel_error_kind("connection".into());
                            ui.set_channel_error_message(
                                "Configure a Resolver connection to load channels.".into(),
                            );
                            ui.set_channel_error_details("Resolver URL is not configured.".into());
                        }
                    });
                    return;
                }
                let categories = match resolver.get_categories(&res_url) {
                    Ok(categories) => categories,
                    Err(error) => {
                        let details = resolver_error_summary(&error);
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.set_channel_error_kind("categories".into());
                                ui.set_channel_error_message(
                                    "Could not load channel categories. Check the Resolver connection and retry.".into(),
                                );
                                ui.set_channel_error_details(details.into());
                            }
                        });
                        return;
                    }
                };
                let hidden = local_store.get_hidden_channels().unwrap_or_default();
                let mut ids = HashMap::new();
                let mut names = vec![slint::SharedString::from("All Channels")];
                for category in categories {
                    if !hidden.iter().any(|id| id == &category.id) {
                        names.push(category.title.clone().into());
                    }
                    ids.insert(category.title, category.id);
                }
                *category_ids.lock().unwrap() = ids;
                let names_clone = names.clone();
                let local_store_clone = local_store.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_channels_categories(Rc::new(slint::VecModel::from(names_clone.clone())).into());
                        if ui.get_channel_error_kind() == "categories" {
                            ui.set_channel_error_kind("".into());
                            ui.set_channel_error_message("".into());
                            ui.set_channel_error_details("".into());
                        }

                        // Restore saved single channel if known and not already selected
                        let active = ui.get_active_channel_category().to_string();
                        if active.is_empty() {
                            if let Ok(Some(saved)) = local_store_clone.get_selected_channel() {
                                if names_clone.iter().any(|n| n.as_str().eq_ignore_ascii_case(&saved) && !n.as_str().eq_ignore_ascii_case("all channels")) {
                                    ui.invoke_select_channel_category(saved.into());
                                } else {
                                    // Saved channel no longer available; clear it and remain on "Choose a channel"
                                    local_store_clone.set_selected_channel(None).ok();
                                }
                            }
                        }
                    }
                });
            });
        });
    }

    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let current_channel_id = current_channel_id.clone();
        let category_ids = category_ids.clone();
        let channel_items_cache = channel_items_cache.clone();
        let channel_cache_map = channel_cache_map.clone();
        ui.on_hide_current_channel(move || {
            let Some(ui) = weak.upgrade() else { return };
            let channel_id = current_channel_id.lock().unwrap().clone();
            if channel_id == "main" || channel_id.is_empty() || local_store.hide_channel(&channel_id).is_err() {
                return;
            }

            local_store.set_selected_channel(None).ok();
            channel_cache_map.lock().unwrap().remove(&channel_id);

            let mut hidden = local_store.get_hidden_channels().unwrap_or_default();
            hidden.sort();
            let hidden_model = Rc::new(slint::VecModel::from(
                hidden
                    .iter()
                    .cloned()
                    .map(slint::SharedString::from)
                    .collect::<Vec<_>>(),
            ));
            ui.set_hidden_channels_list(hidden_model.into());

            let ids = category_ids.lock().unwrap();
            let mut visible_categories = vec![slint::SharedString::from("All Channels")];
            visible_categories.extend(
                ids.iter()
                    .filter(|(_, id)| !hidden.iter().any(|hidden_id| hidden_id == *id))
                    .map(|(title, _)| slint::SharedString::from(title.as_str())),
            );
            visible_categories[1..].sort_by_key(|title| title.to_lowercase());
            drop(ids);
            ui.set_channels_categories(Rc::new(slint::VecModel::from(visible_categories)).into());
            // Return to channel chooser rather than forcing All Channels
            *current_channel_id.lock().unwrap() = String::new();
            *channel_items_cache.lock().unwrap() = Vec::new();
            ui.set_active_channel_category("".into());
            set_channel_catalog(&ui, vec![]);
            ui.set_channel_loading(false);
        });
    }

    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let channel_items_cache = channel_items_cache.clone();
        let channel_generation = channel_generation.clone();
        let channel_sort_mode = channel_sort_mode.clone();
        let channel_duration_filter = channel_duration_filter.clone();
        ui.on_cycle_channel_sort(move || {
            let Some(ui) = weak.upgrade() else { return };
            let next = {
                let mut mode = channel_sort_mode.lock().unwrap();
                *mode = (*mode + 1) % 3;
                *mode
            };
            ui.set_channel_sort_mode(next as i32);
            let _ = local_store.set_setting(
                "channel_sort_mode",
                match next {
                    1 => "longest",
                    2 => "shortest",
                    _ => "latest",
                },
            );
            let request_id = channel_generation.fetch_add(1, Ordering::SeqCst) + 1;
            ui.set_channel_loading(false);
            let items = filter_sort_channel_items(
                channel_items_cache.lock().unwrap().clone(),
                ui.get_channel_search_query().as_str(),
                *channel_duration_filter.lock().unwrap(),
                next,
            );
            let cards = items.iter().map(tv_item_to_card).collect::<Vec<_>>();
            set_channel_catalog(&ui, cards.clone());
            refresh_channel_thumbnails_async(
                weak.clone(),
                cards,
                channel_items_cache.clone(),
                channel_generation.clone(),
                request_id,
            );
        });
    }

    {
        let weak = ui.as_weak();
        let channel_items_cache = channel_items_cache.clone();
        let channel_generation = channel_generation.clone();
        let channel_sort_mode = channel_sort_mode.clone();
        let channel_duration_filter = channel_duration_filter.clone();
        ui.on_cycle_channel_duration_filter(move || {
            let Some(ui) = weak.upgrade() else { return };
            let next = {
                let mut filter = channel_duration_filter.lock().unwrap();
                *filter = (*filter + 1) % 4;
                *filter
            };
            ui.set_channel_duration_filter(next as i32);
            let request_id = channel_generation.fetch_add(1, Ordering::SeqCst) + 1;
            ui.set_channel_loading(false);
            let items = filter_sort_channel_items(
                channel_items_cache.lock().unwrap().clone(),
                ui.get_channel_search_query().as_str(),
                next,
                *channel_sort_mode.lock().unwrap(),
            );
            let cards = items.iter().map(tv_item_to_card).collect::<Vec<_>>();
            set_channel_catalog(&ui, cards.clone());
            refresh_channel_thumbnails_async(
                weak.clone(),
                cards,
                channel_items_cache.clone(),
                channel_generation.clone(),
                request_id,
            );
        });
    }

    // 1. Select Channel Category
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let resolver = resolver.clone();
        let cached_catalog = cached_catalog.clone();
        let current_channel_id = current_channel_id.clone();
        let category_ids = category_ids.clone();
        let channel_offset_id = channel_offset_id.clone();
        let channel_items_cache = channel_items_cache.clone();
        let channel_generation = channel_generation.clone();
        let channel_sort_mode = channel_sort_mode.clone();
        let channel_duration_filter = channel_duration_filter.clone();

        let channel_cache_map_sel = channel_cache_map.clone();
        ui.on_select_channel_category(move |cat| {
            let Some(ui) = weak.upgrade() else { return };
            let cat_str = cat.to_string();
            if cat_str.is_empty() {
                ui.set_active_channel_category("".into());
                set_channel_catalog(&ui, vec![]);
                ui.set_channel_loading(false);
                return;
            }

            ui.set_active_channel_category(cat.clone());
            ui.set_channel_scroll_y(0.0);
            ui.set_channel_error_message("".into());
            ui.set_channel_error_kind("".into());
            ui.set_channel_error_details("".into());
            ui.set_channel_error_details_visible(false);

            let is_all = cat_str.eq_ignore_ascii_case("all channels");
            let chan_id = if is_all {
                local_store.set_selected_channel(None).ok();
                "main".to_string()
            } else {
                local_store.set_selected_channel(Some(&cat_str)).ok();
                category_ids
                    .lock()
                    .unwrap()
                    .get(&cat_str)
                    .cloned()
                    .unwrap_or_else(|| cat_str.to_lowercase().replace(' ', "_"))
            };
            *current_channel_id.lock().unwrap() = chan_id.clone();
            *channel_offset_id.lock().unwrap() = None;
            let request_id = channel_generation.fetch_add(1, Ordering::SeqCst) + 1;

            // Check cache first for instant display
            let cached_entry = channel_cache_map_sel.lock().unwrap().get(&chan_id).cloned();
            if let Some(entry) = cached_entry {
                *channel_items_cache.lock().unwrap() = entry.items.clone();
                *channel_offset_id.lock().unwrap() = entry.next_offset;
                let display_items = filter_sort_channel_items(
                    filter_catalog_items(entry.items.clone(), &local_store),
                    ui.get_channel_search_query().as_str(),
                    *channel_duration_filter.lock().unwrap(),
                    *channel_sort_mode.lock().unwrap(),
                );
                let cards = display_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
                set_channel_catalog(&ui, cards.clone());
                ui.set_channel_has_more(entry.has_more);
                ui.set_channel_loading(false);
                refresh_channel_thumbnails_async(
                    weak.clone(),
                    cards,
                    channel_items_cache.clone(),
                    channel_generation.clone(),
                    request_id,
                );
                return;
            }

            // Not in cache: immediately clear previous cards so user does NOT see old items under new header!
            set_channel_catalog(&ui, vec![]);
            *channel_items_cache.lock().unwrap() = Vec::new();
            ui.set_channel_loading(true);
            ui.set_channel_search_query("".into());

            let weak = weak.clone();
            let local_store = local_store.clone();
            let credential_store = credential_store.clone();
            let resolver = resolver.clone();
            let cached_catalog = cached_catalog.clone();
            let category_ids = category_ids.clone();
            let current_channel_id = current_channel_id.clone();
            let channel_offset_id = channel_offset_id.clone();
            let channel_items_cache = channel_items_cache.clone();
            let channel_cache_map = channel_cache_map_sel.clone();
            let channel_generation = channel_generation.clone();
            let channel_sort_mode = channel_sort_mode.clone();
            let channel_duration_filter = channel_duration_filter.clone();

            std::thread::spawn(move || {
                let res_url = local_store
                    .get_setting("resolver_base_url")
                    .ok()
                    .flatten()
                    .or_else(|| credential_store.get().ok().flatten().map(|s| s.server_url))
                    .unwrap_or_default();

                if res_url.is_empty() {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            if channel_generation.load(Ordering::SeqCst) == request_id
                                && *current_channel_id.lock().unwrap() == chan_id
                            {
                                ui.set_channel_loading(false);
                                ui.set_channel_error_kind("connection".into());
                                ui.set_channel_error_message(
                                    "Resolver URL is not configured. Add it in Settings to load channel videos.".into(),
                                );
                            }
                        }
                    });
                    return;
                }

                if is_all {
                    let categories = match resolver.get_categories(&res_url) {
                        Ok(cats) => cats,
                        Err(err) => {
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = weak.upgrade() {
                                    if channel_generation.load(Ordering::SeqCst) == request_id
                                        && *current_channel_id.lock().unwrap() == chan_id
                                    {
                                        ui.set_channel_loading(false);
                                        ui.set_channel_error_kind(resolver_error_kind(&err).into());
                                        ui.set_channel_error_message(format!(
                                            "Could not load channel categories: {}.",
                                            resolver_error_summary(&err)
                                        ).into());
                                    }
                                }
                            });
                            return;
                        }
                    };

                    let hidden = local_store.get_hidden_channels().unwrap_or_default();
                    let mut ids = category_ids.lock().unwrap();
                    for category in &categories {
                        ids.insert(category.title.clone(), category.id.clone());
                    }
                    drop(ids);

                    let visible_categories: Vec<_> = categories
                        .into_iter()
                        .filter(|category| !hidden.iter().any(|id| id == &category.id))
                        .collect();

                    let mut items = Vec::new();
                    let mut seen_ids = std::collections::HashSet::new();

                    // Stream results incrementally from each category
                    for cat in visible_categories.iter().take(6) {
                        if channel_generation.load(Ordering::SeqCst) != request_id
                            || *current_channel_id.lock().unwrap() != "main"
                        {
                            return;
                        }

                        let cat_videos = if let Some(cached) = channel_cache_map.lock().unwrap().get(&cat.id) {
                            cached.items.clone()
                        } else {
                            let fetched = if let Ok(history) = resolver.get_channel_history(&res_url, &cat.id, 8, None) {
                                history.videos.into_iter().map(TvItem::from).collect::<Vec<_>>()
                            } else if let Ok(videos) = resolver.get_category_videos(&res_url, &cat.id, 1, "recent", None) {
                                videos.into_iter().map(TvItem::from).collect::<Vec<_>>()
                            } else {
                                Vec::new()
                            };
                            if !fetched.is_empty() {
                                channel_cache_map.lock().unwrap().insert(cat.id.clone(), ChannelCacheEntry {
                                    items: fetched.clone(),
                                    next_offset: None,
                                    has_more: false,
                                });
                            }
                            fetched
                        };

                        for item in cat_videos {
                            if seen_ids.insert(item.id.clone()) {
                                items.push(item);
                            }
                        }

                        // Incremental update to UI as each channel finishes
                        let partial_display = filter_sort_channel_items(
                            filter_catalog_items(items.clone(), &local_store),
                            "",
                            *channel_duration_filter.lock().unwrap(),
                            *channel_sort_mode.lock().unwrap(),
                        );
                        let weak_partial = weak.clone();
                        let generation_partial = channel_generation.clone();
                        let current_channel_partial = current_channel_id.clone();

                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak_partial.upgrade() {
                                if generation_partial.load(Ordering::SeqCst) == request_id
                                    && *current_channel_partial.lock().unwrap() == "main"
                                {
                                    let cards = partial_display.iter().map(tv_item_to_card).collect::<Vec<_>>();
                                    set_channel_catalog(&ui, cards);
                                    ui.set_channel_loading(false);
                                }
                            }
                        });
                    }

                    if channel_generation.load(Ordering::SeqCst) != request_id
                        || *current_channel_id.lock().unwrap() != "main"
                    {
                        return;
                    }

                    let filtered = filter_catalog_items(items, &local_store);
                    *channel_offset_id.lock().unwrap() = None;
                    *channel_items_cache.lock().unwrap() = filtered.clone();

                    channel_cache_map.lock().unwrap().insert("main".to_string(), ChannelCacheEntry {
                        items: filtered.clone(),
                        next_offset: None,
                        has_more: false,
                    });

                    // Merge into cached_catalog
                    {
                        let mut cat_lock = cached_catalog.lock().unwrap();
                        for it in &filtered {
                            if !cat_lock.iter().any(|x| x.id == it.id) {
                                cat_lock.push(it.clone());
                            }
                        }
                    }

                    let is_empty = filtered.is_empty() && !visible_categories.is_empty();
                    let display_items = filter_sort_channel_items(
                        filtered,
                        "",
                        *channel_duration_filter.lock().unwrap(),
                        *channel_sort_mode.lock().unwrap(),
                    );
                    let weak_final = weak.clone();
                    let generation_final = channel_generation.clone();
                    let channel_id_final = current_channel_id.clone();
                    let refresh_items = channel_items_cache.clone();

                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(ui) = weak_final.upgrade() else { return };
                        if generation_final.load(Ordering::SeqCst) != request_id
                            || *channel_id_final.lock().unwrap() != "main"
                        {
                            return;
                        }
                        let final_cards = display_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
                        set_channel_catalog(&ui, final_cards.clone());
                        ui.set_channel_has_more(false);
                        ui.set_channel_loading(false);
                        if is_empty {
                            ui.set_channel_error_kind("empty".into());
                            ui.set_channel_error_message("No videos available across channel categories.".into());
                        } else {
                            ui.set_channel_error_kind("".into());
                            ui.set_channel_error_message("".into());
                        }
                        refresh_channel_thumbnails_async(
                            ui.as_weak(),
                            final_cards,
                            refresh_items,
                            generation_final,
                            request_id,
                        );
                    });
                } else {
                    // Single channel fetch
                    let mut effective_channel_id = chan_id.clone();
                    if let Ok(categories) = resolver.get_categories(&res_url) {
                        let mut ids = category_ids.lock().unwrap();
                        for category in &categories {
                            ids.insert(category.title.clone(), category.id.clone());
                        }
                        if let Some(category) = categories
                            .iter()
                            .find(|category| category.title.eq_ignore_ascii_case(&cat_str))
                        {
                            effective_channel_id = category.id.clone();
                        }
                    }

                    let mut items = Vec::new();
                    let mut next_offset = None;
                    let mut has_more = false;
                    let mut error_kind = "";
                    let mut error_message = None;

                    match resolver.get_channel_history(&res_url, &effective_channel_id, 20, None) {
                        Ok(history) => {
                            next_offset = history.next_offset_id;
                            has_more = history.has_more;
                            items.extend(history.videos.into_iter().map(TvItem::from));
                        }
                        Err(history_error) => {
                            match resolver.get_category_videos(&res_url, &effective_channel_id, 1, "recent", None) {
                                Ok(videos) => items.extend(videos.into_iter().map(TvItem::from)),
                                Err(fallback_error) => {
                                    error_kind = resolver_error_kind(&history_error);
                                    error_message = Some(format!(
                                        "Could not load channel history ({}); fallback request failed ({}).",
                                        resolver_error_summary(&history_error),
                                        resolver_error_summary(&fallback_error)
                                    ));
                                }
                            }
                        }
                    }

                    let duration_filter = *channel_duration_filter.lock().unwrap();
                    let sort_mode = *channel_sort_mode.lock().unwrap();
                    let mut empty_page_attempts = 0;
                    loop {
                        let visible = filter_sort_channel_items(
                            filter_catalog_items(items.clone(), &local_store),
                            "",
                            duration_filter,
                            sort_mode,
                        );
                        if !visible.is_empty() || !has_more || empty_page_attempts >= 3 {
                            break;
                        }
                        let Some(offset) = next_offset else { break };
                        match resolver.get_channel_history(
                            &res_url,
                            &effective_channel_id,
                            20,
                            Some(offset),
                        ) {
                            Ok(history) => {
                                if history.next_offset_id == Some(offset) {
                                    has_more = false;
                                    break;
                                }
                                next_offset = history.next_offset_id;
                                has_more = history.has_more;
                                items.extend(history.videos.into_iter().map(TvItem::from));
                            }
                            Err(error) => {
                                error_kind = resolver_error_kind(&error);
                                error_message = Some(format!(
                                    "Could not continue past filtered channel items: {}.",
                                    resolver_error_summary(&error)
                                ));
                                has_more = false;
                                break;
                            }
                        }
                        empty_page_attempts += 1;
                    }

                    // Strict currency check: verify BOTH request_id AND current channel ID
                    if channel_generation.load(Ordering::SeqCst) != request_id
                        || *current_channel_id.lock().unwrap() != chan_id
                    {
                        // Cache the result for future visits, but do NOT touch the UI
                        if !items.is_empty() {
                            channel_cache_map.lock().unwrap().insert(chan_id, ChannelCacheEntry {
                                items: filter_catalog_items(items, &local_store),
                                next_offset,
                                has_more,
                            });
                        }
                        return;
                    }

                    let filtered = filter_catalog_items(items, &local_store);
                    *channel_offset_id.lock().unwrap() = next_offset;
                    *channel_items_cache.lock().unwrap() = filtered.clone();

                    channel_cache_map.lock().unwrap().insert(chan_id.clone(), ChannelCacheEntry {
                        items: filtered.clone(),
                        next_offset,
                        has_more,
                    });

                    // Merge into cached_catalog
                    {
                        let mut cat_lock = cached_catalog.lock().unwrap();
                        for it in &filtered {
                            if !cat_lock.iter().any(|x| x.id == it.id) {
                                cat_lock.push(it.clone());
                            }
                        }
                    }

                    let display_items = filter_sort_channel_items(
                        filtered.clone(),
                        "",
                        *channel_duration_filter.lock().unwrap(),
                        *channel_sort_mode.lock().unwrap(),
                    );
                    let refresh_items = channel_items_cache.clone();
                    let refresh_generation = channel_generation.clone();
                    let error_details = error_message.unwrap_or_default();
                    let error_message = if error_kind == "access-denied" {
                        "Access to this channel was denied. Check the Resolver connection and try again.".to_owned()
                    } else {
                        error_details.clone()
                    };

                    slint::invoke_from_event_loop(move || {
                        let Some(ui) = weak.upgrade() else { return };
                        if channel_generation.load(Ordering::SeqCst) != request_id
                            || *current_channel_id.lock().unwrap() != chan_id
                        {
                            return;
                        }
                        let cards = display_items
                            .iter()
                            .map(tv_item_to_card)
                            .collect::<Vec<_>>();
                        set_channel_catalog(&ui, cards.clone());
                        ui.set_channel_error_message(error_message.into());
                        ui.set_channel_error_kind(error_kind.into());
                        ui.set_channel_error_details(error_details.into());
                        ui.set_channel_has_more(has_more);
                        ui.set_channel_loading(false);
                        refresh_channel_thumbnails_async(
                            ui.as_weak(),
                            cards,
                            refresh_items,
                            refresh_generation,
                            request_id,
                        );
                    })
                    .ok();
                }
            });
        });
    }

    // 2. Load More Channel Videos (Pagination)
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let resolver = resolver.clone();
        let cached_catalog = cached_catalog.clone();
        let current_channel_id = current_channel_id.clone();
        let channel_offset_id = channel_offset_id.clone();
        let channel_items_cache = channel_items_cache.clone();
        let channel_generation = channel_generation.clone();
        let channel_sort_mode = channel_sort_mode.clone();
        let channel_duration_filter = channel_duration_filter.clone();

        let channel_cache_map = channel_cache_map.clone();

        ui.on_load_more_channel_videos(move || {
            let Some(ui) = weak.upgrade() else { return };
            let chan_id = current_channel_id.lock().unwrap().clone();
            if chan_id == "main" || chan_id.is_empty() {
                ui.set_channel_loading(false);
                ui.set_channel_has_more(false);
                return;
            }
            ui.set_channel_loading(true);

            let weak = weak.clone();
            let query = ui.get_channel_search_query().to_string();
            let local_store = local_store.clone();
            let credential_store = credential_store.clone();
            let resolver = resolver.clone();
            let cached_catalog = cached_catalog.clone();
            let chan_id = current_channel_id.lock().unwrap().clone();
            let offset = *channel_offset_id.lock().unwrap();
            let channel_offset_id = channel_offset_id.clone();
            let channel_items_cache = channel_items_cache.clone();
            let channel_cache_map = channel_cache_map.clone();
            let current_channel_id = current_channel_id.clone();
            let channel_generation = channel_generation.clone();
            let channel_sort_mode = channel_sort_mode.clone();
            let channel_duration_filter = channel_duration_filter.clone();
            let request_id = channel_generation.load(Ordering::SeqCst);

            std::thread::spawn(move || {
                let res_url = local_store
                    .get_setting("resolver_base_url")
                    .ok()
                    .flatten()
                    .or_else(|| credential_store.get().ok().flatten().map(|s| s.server_url))
                    .unwrap_or_default();

                let mut new_items = Vec::new();
                let mut next_offset = None;
                let mut has_more = false;
                let mut load_error = if res_url.is_empty() {
                    Some((
                        "connection".to_owned(),
                        "Resolver URL is not configured.".to_owned(),
                    ))
                } else {
                    None
                };

                if !res_url.is_empty() {
                    match resolver.get_channel_history(&res_url, &chan_id, 20, offset) {
                        Ok(hist) => {
                            next_offset = hist.next_offset_id;
                            has_more = hist.has_more;
                            for v in hist.videos {
                                new_items.push(TvItem::from(v));
                            }
                        }
                        Err(error) => {
                            load_error = Some((
                                resolver_error_kind(&error).to_owned(),
                                resolver_error_summary(&error),
                            ));
                        }
                    }
                }

                if channel_generation.load(Ordering::SeqCst) != request_id
                    || *current_channel_id.lock().unwrap() != chan_id
                {
                    return;
                }

                let filtered_new = filter_catalog_items(new_items, &local_store);
                *channel_offset_id.lock().unwrap() = next_offset;

                let mut all_channel_items = channel_items_cache.lock().unwrap();
                for item in &filtered_new {
                    if !all_channel_items
                        .iter()
                        .any(|existing| existing.id == item.id)
                    {
                        all_channel_items.push(item.clone());
                    }
                }

                channel_cache_map.lock().unwrap().insert(chan_id.clone(), ChannelCacheEntry {
                    items: all_channel_items.clone(),
                    next_offset,
                    has_more,
                });

                let display_items = filter_sort_channel_items(
                    all_channel_items.clone(),
                    &query,
                    *channel_duration_filter.lock().unwrap(),
                    *channel_sort_mode.lock().unwrap(),
                );
                let refresh_items = channel_items_cache.clone();
                let refresh_generation = channel_generation.clone();
                let error_kind = load_error.as_ref().map(|(kind, _)| kind.clone());
                let error_details = load_error.map(|(_, details)| details).unwrap_or_default();

                // Merge into cached_catalog
                {
                    let mut cat_lock = cached_catalog.lock().unwrap();
                    for it in &filtered_new {
                        if !cat_lock.iter().any(|x| x.id == it.id) {
                            cat_lock.push(it.clone());
                        }
                    }
                }

                let current_channel_id_loop = current_channel_id.clone();
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    if channel_generation.load(Ordering::SeqCst) != request_id
                        || *current_channel_id_loop.lock().unwrap() != chan_id
                    {
                        return;
                    }
                    let cards = display_items
                        .iter()
                        .map(tv_item_to_card)
                        .collect::<Vec<_>>();
                    append_channel_page(&ui, cards.clone());
                    ui.set_channel_has_more(has_more);
                    ui.set_channel_loading(false);
                    if let Some(error_kind) = error_kind {
                        ui.set_channel_error_kind(error_kind.clone().into());
                        ui.set_channel_error_message(if error_kind == "access-denied" {
                            "The Resolver denied access. Check the account and permissions configured for this channel.".into()
                        } else {
                            "Could not load more channel videos. Check the connection and retry.".into()
                        });
                        ui.set_channel_error_details(error_details.into());
                    }
                    refresh_channel_thumbnails_async(
                        ui.as_weak(),
                        cards,
                        refresh_items,
                        refresh_generation,
                        request_id,
                    );
                })
                .ok();
            });
        });
    }

    // 3. Search Channel Videos
    {
        let weak = ui.as_weak();
        let channel_items_cache = channel_items_cache.clone();
        let channel_generation = channel_generation.clone();
        let channel_sort_mode = channel_sort_mode.clone();
        let channel_duration_filter = channel_duration_filter.clone();
        let local_store = local_store.clone();

        ui.on_search_channel_videos(move |query| {
            let Some(ui) = weak.upgrade() else { return };
            let request_id = channel_generation.fetch_add(1, Ordering::SeqCst) + 1;
            let channel_generation = channel_generation.clone();
            let raw_q = query.to_string();
            let trimmed = raw_q.trim();
            if !trimmed.is_empty() {
                let _ = local_store.add_search_query_context("channels", trimmed);
                super::browsing::refresh_recent_searches_for_context(&ui, &local_store);
            }
            let q = trimmed.to_lowercase();
            let all = channel_items_cache.lock().unwrap().clone();
            let filtered = filter_sort_channel_items(
                all,
                &q,
                *channel_duration_filter.lock().unwrap(),
                *channel_sort_mode.lock().unwrap(),
            );
            ui.set_channel_loading(false);

            let cards = filtered.iter().map(tv_item_to_card).collect::<Vec<_>>();
            set_channel_catalog(&ui, cards.clone());
            refresh_channel_thumbnails_async(
                weak.clone(),
                cards,
                channel_items_cache.clone(),
                channel_generation,
                request_id,
            );
        });
    }

    // 4. Initial Channel Population
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let resolver = resolver.clone();
        let cached_catalog = cached_catalog.clone();
        let channel_items_cache = channel_items_cache.clone();
        let category_ids = category_ids.clone();
        let channel_offset_id = channel_offset_id.clone();
        let channel_generation = channel_generation.clone();
        let channel_sort_mode = channel_sort_mode.clone();
        let channel_duration_filter = channel_duration_filter.clone();
        let request_id = channel_generation.load(Ordering::SeqCst);

        std::thread::spawn(move || {
            let res_url = local_store
                .get_setting("resolver_base_url")
                .ok()
                .flatten()
                .or_else(|| credential_store.get().ok().flatten().map(|s| s.server_url))
                .unwrap_or_default();

            let hidden = local_store.get_hidden_channels().unwrap_or_default();
            let mut cat_names = vec!["All Channels".to_string()];
            let mut ids = HashMap::new();
            let mut initial_items = Vec::new();
            let mut next_offset = None;
            let mut has_more = false;
            let mut error_kind = String::new();
            let mut error_details = String::new();
            if res_url.is_empty() {
                error_kind = "connection".into();
                error_details = "Resolver URL is not configured.".into();
            } else {
                match resolver.get_categories(&res_url) {
                    Ok(cats) => {
                        for category in cats {
                            ids.insert(category.title.clone(), category.id.clone());
                            if !hidden.iter().any(|id| id == &category.id) {
                                cat_names.push(category.title);
                            }
                        }
                    }
                    Err(error) => {
                        error_kind = resolver_error_kind(&error).into();
                        error_details = format!(
                            "Could not load channel categories: {}",
                            resolver_error_summary(&error)
                        );
                    }
                }
                match resolver.get_channel_history(&res_url, "main", 20, None) {
                    Ok(history) => {
                        next_offset = history.next_offset_id;
                        has_more = history.has_more;
                        initial_items = history.videos.into_iter().map(TvItem::from).collect();
                    }
                    Err(error) => {
                        error_kind = resolver_error_kind(&error).into();
                        error_details = format!(
                            "Could not load channel history: {}",
                            resolver_error_summary(&error)
                        );
                    }
                }
            }
            let error_message = if error_kind == "access-denied" {
                "The Resolver denied access. Check the account and permissions configured for this channel.".to_owned()
            } else if !error_kind.is_empty() {
                "Channel data could not be loaded. Check the Resolver connection and retry."
                    .to_owned()
            } else {
                String::new()
            };
            initial_items = filter_catalog_items(initial_items, &local_store);
            *category_ids.lock().unwrap() = ids;
            let weak_categories = weak.clone();
            slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak_categories.upgrade() {
                    let model = Rc::new(slint::VecModel::from(
                        cat_names
                            .into_iter()
                            .map(slint::SharedString::from)
                            .collect::<Vec<_>>(),
                    ));
                    ui.set_channels_categories(model.into());
                }
            })
            .ok();
            if channel_generation.load(Ordering::SeqCst) != request_id {
                return;
            }
            *channel_offset_id.lock().unwrap() = next_offset;
            *channel_items_cache.lock().unwrap() = initial_items.clone();
            {
                let mut catalog = cached_catalog.lock().unwrap();
                for item in &initial_items {
                    if !catalog.iter().any(|existing| existing.id == item.id) {
                        catalog.push(item.clone());
                    }
                }
            }
            let display_items = filter_sort_channel_items(
                initial_items,
                "",
                *channel_duration_filter.lock().unwrap(),
                *channel_sort_mode.lock().unwrap(),
            );
            let refresh_items = channel_items_cache.clone();
            let refresh_generation = channel_generation.clone();
            slint::invoke_from_event_loop(move || {
                let Some(ui) = weak.upgrade() else { return };
                if channel_generation.load(Ordering::SeqCst) != request_id {
                    return;
                }
                let cards = display_items
                    .iter()
                    .map(tv_item_to_card)
                    .collect::<Vec<_>>();
                set_channel_catalog(&ui, cards.clone());
                ui.set_channel_has_more(has_more);
                ui.set_channel_loading(false);
                ui.set_channel_error_kind(error_kind.into());
                ui.set_channel_error_message(error_message.into());
                ui.set_channel_error_details(error_details.into());
                refresh_channel_thumbnails_async(
                    ui.as_weak(),
                    cards,
                    refresh_items,
                    refresh_generation,
                    request_id,
                );
            })
            .ok();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn channel_cache_and_currency_invalidation() {
        let cache = Arc::new(Mutex::new(HashMap::<String, ChannelCacheEntry>::new()));
        let current_channel_id = Arc::new(Mutex::new(String::new()));
        let channel_generation = Arc::new(AtomicUsize::new(0));

        // Initial state: empty channel picker
        assert_eq!(*current_channel_id.lock().unwrap(), "");

        // User selects "channel-1" (request 1)
        let req1 = channel_generation.fetch_add(1, Ordering::SeqCst) + 1;
        *current_channel_id.lock().unwrap() = "channel-1".to_string();

        let item1 = TvItem {
            id: "v1".into(),
            title: "Video 1".into(),
            ..Default::default()
        };
        cache.lock().unwrap().insert(
            "channel-1".to_string(),
            ChannelCacheEntry {
                items: vec![item1.clone()],
                next_offset: Some(10),
                has_more: true,
            },
        );

        // User quickly switches to "channel-2" (request 2) before background thread for channel-1 completes
        let req2 = channel_generation.fetch_add(1, Ordering::SeqCst) + 1;
        *current_channel_id.lock().unwrap() = "channel-2".to_string();

        // Background thread for request 1 tries to update UI with channel-1
        let is_valid_req1 = channel_generation.load(Ordering::SeqCst) == req1
            && *current_channel_id.lock().unwrap() == "channel-1";
        assert!(!is_valid_req1, "Stale request 1 must be rejected");

        // Background thread for request 2 checks currency
        let is_valid_req2 = channel_generation.load(Ordering::SeqCst) == req2
            && *current_channel_id.lock().unwrap() == "channel-2";
        assert!(is_valid_req2, "Current request 2 must be accepted");

        // Switching back to channel-1 hits cache instantly
        let cached = cache.lock().unwrap().get("channel-1").cloned();
        assert!(cached.is_some());
        let cached_entry = cached.unwrap();
        assert_eq!(cached_entry.items.len(), 1);
        assert_eq!(cached_entry.items[0].id, "v1");
        assert_eq!(cached_entry.next_offset, Some(10));
        assert!(cached_entry.has_more);
    }

    #[test]
    fn channel_filter_sort_modes() {
        let v1 = TvItem {
            id: "1".into(),
            title: "Alpha Video".into(),
            runtime_ticks: Some(2 * 60 * 10_000_000), // 2 mins -> short (1..=14)
            ..Default::default()
        };
        let v2 = TvItem {
            id: "2".into(),
            title: "Beta Long Movie".into(),
            runtime_ticks: Some(60 * 60 * 10_000_000), // 60 mins -> long (>40)
            ..Default::default()
        };
        let v3 = TvItem {
            id: "3".into(),
            title: "Gamma Clip".into(),
            runtime_ticks: Some(10 * 60 * 10_000_000), // 10 mins -> short (1..=14)
            ..Default::default()
        };

        let items = vec![v1.clone(), v2.clone(), v3.clone()];

        // Filter by duration: short (1..=14m) -> v1 (2m) and v3 (10m)
        let filtered_short = filter_sort_channel_items(items.clone(), "", 1, 0);
        assert_eq!(filtered_short.len(), 2);
        assert!(filtered_short.iter().any(|i| i.id == "1"));
        assert!(filtered_short.iter().any(|i| i.id == "3"));

        // Sort by longest -> v2 (60m), v3 (10m), v1 (2m)
        let sorted_longest = filter_sort_channel_items(items.clone(), "", 0, 1);
        assert_eq!(sorted_longest[0].id, "2");
        assert_eq!(sorted_longest[1].id, "3");
        assert_eq!(sorted_longest[2].id, "1");

        // Search query filter
        let searched = filter_sort_channel_items(items.clone(), "Long", 0, 0);
        assert_eq!(searched.len(), 1);
        assert_eq!(searched[0].id, "2");
    }
}
