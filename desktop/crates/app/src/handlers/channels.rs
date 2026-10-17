use crate::MainWindow;
use crate::catalog::{filter_catalog_items, sample_catalog_tv_items, tv_item_to_card};
use crate::context::AppContext;
use adult_hub_client::TvItem;
use slint::ComponentHandle;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub fn install_stage8_handlers(ui: &MainWindow, ctx: &AppContext) {
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let resolver = ctx.resolver.clone();
    let cached_catalog = ctx.cached_catalog.clone();

    let current_channel_id = Arc::new(Mutex::new("all".to_string()));
    let channel_offset_id = Arc::new(Mutex::new(None::<i64>));
    let channel_items_cache = Arc::new(Mutex::new(Vec::<TvItem>::new()));

    // 1. Select Channel Category
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let resolver = resolver.clone();
        let cached_catalog = cached_catalog.clone();
        let current_channel_id = current_channel_id.clone();
        let channel_offset_id = channel_offset_id.clone();
        let channel_items_cache = channel_items_cache.clone();

        ui.on_select_channel_category(move |cat| {
            let Some(ui) = weak.upgrade() else { return };
            let cat_str = cat.to_string();
            ui.set_active_channel_category(cat.clone());
            ui.set_channel_loading(true);
            *current_channel_id.lock().unwrap() = cat_str.clone();
            *channel_offset_id.lock().unwrap() = None;

            let weak = weak.clone();
            let local_store = local_store.clone();
            let credential_store = credential_store.clone();
            let resolver = resolver.clone();
            let cached_catalog = cached_catalog.clone();
            let channel_offset_id = channel_offset_id.clone();
            let channel_items_cache = channel_items_cache.clone();

            std::thread::spawn(move || {
                let res_url = local_store
                    .get_setting("resolver_base_url")
                    .ok()
                    .flatten()
                    .or_else(|| credential_store.get().ok().flatten().map(|s| s.server_url))
                    .unwrap_or_default();

                let mut items = Vec::new();
                let mut next_offset = None;

                if !res_url.is_empty() {
                    let chan_id = if cat_str.to_lowercase().contains("all") {
                        "main".to_string()
                    } else {
                        cat_str.to_lowercase().replace(' ', "_")
                    };
                    if let Ok(hist) = resolver.get_channel_history(&res_url, &chan_id, 20, None) {
                        next_offset = hist.next_offset_id;
                        for v in hist.videos {
                            items.push(TvItem::from(v));
                        }
                    } else if let Ok(cat_vids) =
                        resolver.get_category_videos(&res_url, &chan_id, 1, "recent", None)
                    {
                        for v in cat_vids {
                            items.push(TvItem::from(v));
                        }
                    }
                }

                if items.is_empty() {
                    let cat_filter = cat_str.to_lowercase();
                    let pool = cached_catalog.lock().unwrap().clone();
                    items = pool
                        .into_iter()
                        .filter(|i| {
                            if cat_filter.contains("4k") {
                                i.height.is_some_and(|h| h >= 2160)
                                    || i.title.to_lowercase().contains("4k")
                            } else if cat_filter.contains("vr") {
                                i.genres.iter().any(|g| g.to_lowercase().contains("vr"))
                                    || i.title.to_lowercase().contains("vr")
                            } else if cat_filter.contains("highlight") {
                                i.runtime_ticks.is_some_and(|t| t < 15 * 60 * 10_000_000)
                            } else if cat_filter.contains("exclusive") {
                                i.studios.iter().any(|s| !s.is_empty())
                            } else {
                                true
                            }
                        })
                        .take(20)
                        .collect();
                }

                let filtered = filter_catalog_items(items, &local_store);
                *channel_offset_id.lock().unwrap() = next_offset;
                *channel_items_cache.lock().unwrap() = filtered.clone();

                // Merge into cached_catalog
                {
                    let mut cat_lock = cached_catalog.lock().unwrap();
                    for it in &filtered {
                        if !cat_lock.iter().any(|x| x.id == it.id) {
                            cat_lock.push(it.clone());
                        }
                    }
                }

                let cards = filtered.iter().map(tv_item_to_card).collect::<Vec<_>>();
                let has_more = next_offset.is_some() || filtered.len() >= 10;

                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    let model = Rc::new(slint::VecModel::from(cards));
                    ui.set_channel_items(model.into());
                    ui.set_channel_has_more(has_more);
                    ui.set_channel_loading(false);
                })
                .ok();
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

        ui.on_load_more_channel_videos(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_channel_loading(true);

            let weak = weak.clone();
            let local_store = local_store.clone();
            let credential_store = credential_store.clone();
            let resolver = resolver.clone();
            let cached_catalog = cached_catalog.clone();
            let cat_str = current_channel_id.lock().unwrap().clone();
            let offset = *channel_offset_id.lock().unwrap();
            let channel_offset_id = channel_offset_id.clone();
            let channel_items_cache = channel_items_cache.clone();

            std::thread::spawn(move || {
                let res_url = local_store
                    .get_setting("resolver_base_url")
                    .ok()
                    .flatten()
                    .or_else(|| credential_store.get().ok().flatten().map(|s| s.server_url))
                    .unwrap_or_default();

                let mut new_items = Vec::new();
                let mut next_offset = None;

                if !res_url.is_empty() {
                    let chan_id = if cat_str.to_lowercase().contains("all") {
                        "main".to_string()
                    } else {
                        cat_str.to_lowercase().replace(' ', "_")
                    };
                    if let Ok(hist) = resolver.get_channel_history(&res_url, &chan_id, 20, offset) {
                        next_offset = hist.next_offset_id;
                        for v in hist.videos {
                            new_items.push(TvItem::from(v));
                        }
                    }
                }

                if new_items.is_empty() {
                    let pool = sample_catalog_tv_items();
                    let current_count = channel_items_cache.lock().unwrap().len();
                    new_items = pool.into_iter().skip(current_count % 3).take(5).collect();
                }

                let filtered_new = filter_catalog_items(new_items, &local_store);
                *channel_offset_id.lock().unwrap() = next_offset;

                let mut all_channel_items = channel_items_cache.lock().unwrap();
                all_channel_items.extend(filtered_new.clone());
                let cards = all_channel_items
                    .iter()
                    .map(tv_item_to_card)
                    .collect::<Vec<_>>();

                // Merge into cached_catalog
                {
                    let mut cat_lock = cached_catalog.lock().unwrap();
                    for it in &filtered_new {
                        if !cat_lock.iter().any(|x| x.id == it.id) {
                            cat_lock.push(it.clone());
                        }
                    }
                }

                let has_more = next_offset.is_some() || (all_channel_items.len() < 50);

                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    let model = Rc::new(slint::VecModel::from(cards));
                    ui.set_channel_items(model.into());
                    ui.set_channel_has_more(has_more);
                    ui.set_channel_loading(false);
                })
                .ok();
            });
        });
    }

    // 3. Search Channel Videos
    {
        let weak = ui.as_weak();
        let channel_items_cache = channel_items_cache.clone();

        ui.on_search_channel_videos(move |query| {
            let Some(ui) = weak.upgrade() else { return };
            let q = query.to_string().trim().to_lowercase();
            let all = channel_items_cache.lock().unwrap().clone();
            let filtered: Vec<TvItem> = if q.is_empty() {
                all
            } else {
                all.into_iter()
                    .filter(|i| {
                        i.title.to_lowercase().contains(&q)
                            || i.overview
                                .as_ref()
                                .is_some_and(|o| o.to_lowercase().contains(&q))
                            || i.studios.iter().any(|s| s.to_lowercase().contains(&q))
                            || i.people.iter().any(|p| p.name.to_lowercase().contains(&q))
                    })
                    .collect()
            };

            let cards = filtered.iter().map(tv_item_to_card).collect::<Vec<_>>();
            let model = Rc::new(slint::VecModel::from(cards));
            ui.set_channel_items(model.into());
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

        std::thread::spawn(move || {
            let res_url = local_store
                .get_setting("resolver_base_url")
                .ok()
                .flatten()
                .or_else(|| credential_store.get().ok().flatten().map(|s| s.server_url))
                .unwrap_or_default();

            if !res_url.is_empty() {
                if let Ok(cats) = resolver.get_categories(&res_url) {
                    if !cats.is_empty() {
                        let mut cat_names = vec!["All Channels".to_string()];
                        for c in cats {
                            cat_names.push(c.title);
                        }
                        let weak2 = weak.clone();
                        slint::invoke_from_event_loop(move || {
                            let Some(ui) = weak2.upgrade() else { return };
                            let cat_model = Rc::new(slint::VecModel::from(
                                cat_names
                                    .into_iter()
                                    .map(slint::SharedString::from)
                                    .collect::<Vec<_>>(),
                            ));
                            ui.set_channels_categories(cat_model.into());
                        })
                        .ok();
                    }
                }
            }

            // Populate initial videos for default channel
            let pool = cached_catalog.lock().unwrap().clone();
            let initial_items: Vec<TvItem> = pool.into_iter().take(15).collect();
            *channel_items_cache.lock().unwrap() = initial_items.clone();
            let cards = initial_items
                .iter()
                .map(tv_item_to_card)
                .collect::<Vec<_>>();
            let weak2 = weak.clone();
            slint::invoke_from_event_loop(move || {
                let Some(ui) = weak2.upgrade() else { return };
                let model = Rc::new(slint::VecModel::from(cards));
                ui.set_channel_items(model.into());
                ui.set_channel_has_more(true);
            })
            .ok();
        });
    }
}
