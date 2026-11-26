use crate::catalog::tv_item_to_card_for_server;
use crate::{HomeCategoryOption, HomeCategoryShelfData, MainWindow, VideoCardData};
use adult_hub_client::TvItem;
use adult_hub_storage::LocalStore;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub(super) fn set_home_shelves(ui: &MainWindow, shelves: &[Vec<VideoCardData>]) {
    let to_model = |items: &[VideoCardData]| Rc::new(slint::VecModel::from(items.to_vec())).into();
    ui.set_home_resume_items(to_model(
        shelves.first().map(Vec::as_slice).unwrap_or_default(),
    ));
    ui.set_home_history_items(to_model(
        shelves.get(1).map(Vec::as_slice).unwrap_or_default(),
    ));
    ui.set_home_latest_items(to_model(
        shelves.get(2).map(Vec::as_slice).unwrap_or_default(),
    ));
    ui.set_home_random_items(to_model(
        shelves.get(3).map(Vec::as_slice).unwrap_or_default(),
    ));
    ui.set_home_performer_items(to_model(
        shelves.get(4).map(Vec::as_slice).unwrap_or_default(),
    ));
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
pub struct PersistedHomeCache {
    pub all_items: Vec<TvItem>,
    pub shelves: Vec<Vec<TvItem>>,
    pub category_shelves: Vec<(String, Vec<TvItem>, String)>,
}

pub fn restore_saved_home_data(
    ui: &MainWindow,
    local_store: &LocalStore,
    server_url: &str,
    home_catalog: &Arc<Mutex<Vec<TvItem>>>,
) -> bool {
    let Ok(Some(json)) = local_store.get_setting("home_shelves_cache_v1") else {
        return false;
    };
    let Ok(cache) = serde_json::from_str::<PersistedHomeCache>(&json) else {
        return false;
    };
    if cache.shelves.iter().all(|s| s.is_empty()) && cache.category_shelves.is_empty() {
        return false;
    }

    *home_catalog
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = cache.all_items;

    let shelves = cache
        .shelves
        .iter()
        .map(|items| {
            items
                .iter()
                .map(|item| tv_item_to_card_for_server(item, server_url))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    set_home_shelves(ui, &shelves);

    let ui_category_shelves = cache
        .category_shelves
        .into_iter()
        .map(|(name, items, error_message)| {
            let cards = items
                .iter()
                .map(|item| tv_item_to_card_for_server(item, server_url))
                .collect::<Vec<_>>();
            HomeCategoryShelfData {
                name: name.into(),
                items: Rc::new(slint::VecModel::from(cards)).into(),
                error_message: error_message.into(),
            }
        })
        .collect::<Vec<_>>();
    ui.set_home_category_shelves(Rc::new(slint::VecModel::from(ui_category_shelves)).into());
    crate::catalog::refresh_home_thumbnails_async(ui.as_weak(), None);
    true
}

pub(super) fn set_home_category_options(
    ui: &MainWindow,
    categories: &[TvItem],
    selected: &[String],
) {
    let options = categories
        .iter()
        .map(|category| HomeCategoryOption {
            id: category.id.clone().into(),
            name: category.title.clone().into(),
            selected: selected.iter().any(|id| id == &category.id),
        })
        .collect::<Vec<_>>();
    ui.set_home_category_options(Rc::new(slint::VecModel::from(options)).into());
}

pub(super) fn resolve_home_category_ids(
    categories: &[TvItem],
    local_store: &LocalStore,
) -> Vec<String> {
    let saved = local_store.get_home_categories().ok().flatten();
    let available = categories
        .iter()
        .map(|category| category.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let selected = match saved.as_ref() {
        Some(saved) if saved.is_empty() => Vec::new(),
        Some(saved) => {
            let valid = saved
                .iter()
                .filter(|id| available.contains(id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            if valid.is_empty() {
                categories
                    .iter()
                    .take(2)
                    .map(|category| category.id.clone())
                    .collect()
            } else {
                valid
            }
        }
        None => categories
            .iter()
            .take(2)
            .map(|category| category.id.clone())
            .collect(),
    };
    if saved.as_ref() != Some(&selected) && !categories.is_empty() {
        let _ = local_store.set_home_categories(Some(&selected));
    }
    selected
}
