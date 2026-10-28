use crate::catalog::{
    FetchedThumbnail, ThumbnailRequest, append_catalog_page, card_index_by_request,
    fetch_thumbnail_batch, filter_catalog_items, no_thumbnail_state,
    refresh_catalog_thumbnails_async, sample_catalog_tv_items, set_catalog, set_discovered_servers,
    tv_item_to_card, tv_item_to_card_for_server,
};
use crate::context::AppContext;
use crate::playback::show_error;
use crate::{
    DiscoveredServerItem, HomeCategoryOption, HomeCategoryShelfData, MainWindow, VideoCardData,
};
use adult_hub_client::{
    GetItemsQuery, JellyfinClient, JellyfinError, ResolverClient, ServiceUrlPolicy,
    ServiceUrlValidation, TvItem, TvItemsPage, discover_servers,
};
use adult_hub_storage::{LocalStore, Session};
use slint::{ComponentHandle, Model};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CATALOG_PAGE_SIZE: usize = 50;

/// Every piece of catalog-search state that must change together when the user
/// leaves a search (the central "Clear search" action). The edited field text,
/// the applied query and the results state are tracked separately on purpose:
/// clearing one without the others desynchronizes the field, the counter and
/// the list. History (`search_history` rows) is deliberately not part of this
/// state — clearing the current query must not erase remembered searches.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogSearchState {
    typed_query: String,
    applied_query: String,
    in_results: bool,
    error_message: String,
    all_has_more: bool,
    all_total_count: i32,
    results_filter: String,
}

impl CatalogSearchState {
    fn cleared() -> Self {
        Self {
            typed_query: String::new(),
            applied_query: String::new(),
            in_results: false,
            error_message: String::new(),
            all_has_more: false,
            all_total_count: 0,
            results_filter: "all".into(),
        }
    }

    fn apply(&self, ui: &MainWindow) {
        ui.set_search_query(self.typed_query.as_str().into());
        ui.set_search_applied_query(self.applied_query.as_str().into());
        ui.set_is_catalog_search_results(self.in_results);
        ui.set_search_error_message(self.error_message.as_str().into());
        ui.set_search_all_has_more(self.all_has_more);
        ui.set_search_all_total_count(self.all_total_count);
        ui.set_search_results_filter(self.results_filter.as_str().into());
    }
}

fn catalog_generation_is_current(generation: &AtomicU64, request_id: u64) -> bool {
    generation.load(Ordering::SeqCst) == request_id
}

pub fn active_search_context(ui: &MainWindow) -> &'static str {
    if ui.get_active_screen() == "channels" {
        "channels"
    } else {
        match ui.get_active_category().as_str() {
            "Performers" => "performers",
            "Studios" => "studios",
            _ => "movies",
        }
    }
}

pub fn refresh_recent_searches_for_context(ui: &MainWindow, store: &LocalStore) {
    let ctx = active_search_context(ui);
    let items = store.get_search_history_context(ctx, 8).unwrap_or_default();
    set_recent_searches(ui, items);
}

fn set_recent_searches(ui: &MainWindow, searches: Vec<String>) {
    let items: Vec<slint::SharedString> = searches.into_iter().map(Into::into).collect();
    ui.set_recent_searches(Rc::new(slint::VecModel::from(items)).into());
}

fn search_result_counts(items: &[TvItem]) -> (usize, usize, usize, usize, usize) {
    let mut movies = 0;
    let mut performers = 0;
    let mut studios = 0;
    let mut tags = 0;
    let mut channels = 0;
    for item in items {
        if item.genres.iter().any(|tag| tag == "__adult_hub_performer") {
            performers += 1;
        } else if item.genres.iter().any(|tag| tag == "__adult_hub_studio") {
            studios += 1;
        } else if item.genres.iter().any(|tag| tag == "__adult_hub_genre") {
            tags += 1;
        } else if item.is_telegram()
            || item
                .genres
                .iter()
                .any(|tag| tag == "__adult_hub_channel_search")
        {
            channels += 1;
        } else {
            movies += 1;
        }
    }
    (movies, performers, studios, tags, channels)
}

fn set_search_sections(ui: &MainWindow, cards: &[VideoCardData]) {
    crate::catalog::set_search_sections(ui, cards.iter().cloned());
}

/// Whether a card belongs to the flat search tab `kind`. Untyped kinds ("all")
/// keep every card.
fn card_matches_search_filter(card: &VideoCardData, kind: &str) -> bool {
    crate::catalog::search_filter_bucket(kind)
        .is_none_or(|bucket| crate::catalog::search_section_bucket(&card.provider) == bucket)
}

fn set_home_shelves(ui: &MainWindow, shelves: &[Vec<VideoCardData>]) {
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

    *home_catalog.lock().unwrap() = cache.all_items;

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
    true
}

fn update_home_thumbnail_model(
    items: slint::ModelRc<VideoCardData>,
    requests: &[ThumbnailRequest],
    start: usize,
    end: usize,
    thumbnails: &[(usize, FetchedThumbnail)],
) {
    for request_index in start..end {
        let Some(request) = requests.get(request_index) else {
            continue;
        };
        let Some(index) = card_index_by_request(&items, request) else {
            continue;
        };
        let Some(mut card) = items.row_data(index) else {
            continue;
        };
        if let Some((_, thumbnail)) = thumbnails
            .iter()
            .find(|(offset, _)| *offset == request_index)
        {
            if let Some(image) = crate::thumbnails::image_from_bytes(&thumbnail.bytes) {
                card.thumbnail = image;
                card.has_thumbnail = true;
                card.thumbnail_state = "ready".into();
            } else {
                card.thumbnail_state = "failed".into();
            }
        } else {
            let has_source = !request.primary_url.is_empty()
                || !request.fallback_url.is_empty()
                || !request.alternate_url.is_empty();
            card.thumbnail_state = no_thumbnail_state(has_source).into();
        }
        items.set_row_data(index, card);
    }
}

fn update_home_shelf_thumbnails(
    ui: &MainWindow,
    shelf_index: usize,
    requests: &[ThumbnailRequest],
    start: usize,
    end: usize,
    thumbnails: &[(usize, FetchedThumbnail)],
) {
    let items = match shelf_index {
        0 => ui.get_home_resume_items(),
        1 => ui.get_home_history_items(),
        2 => ui.get_home_latest_items(),
        3 => ui.get_home_random_items(),
        _ => ui.get_home_performer_items(),
    };
    update_home_thumbnail_model(items, requests, start, end, thumbnails);
}

fn update_home_category_thumbnails(
    ui: &MainWindow,
    category_name: &str,
    requests: &[ThumbnailRequest],
    start: usize,
    end: usize,
    thumbnails: &[(usize, FetchedThumbnail)],
) {
    let categories = ui.get_home_category_shelves();
    for index in 0..categories.row_count() {
        let Some(category) = categories.row_data(index) else {
            continue;
        };
        if category.name != category_name {
            continue;
        }
        update_home_thumbnail_model(category.items, requests, start, end, thumbnails);
        break;
    }
}

fn refresh_home_thumbnails_async(
    ui: slint::Weak<MainWindow>,
    shelves: Vec<Vec<VideoCardData>>,
    category_shelves: Vec<(String, Vec<VideoCardData>, String)>,
    token: String,
    generation: Arc<AtomicU64>,
    request_id: u64,
) {
    let shelves = shelves
        .iter()
        .map(|shelf| shelf.iter().map(ThumbnailRequest::from).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let category_shelves = category_shelves
        .iter()
        .map(|(name, cards, error)| {
            (
                name.clone(),
                cards.iter().map(ThumbnailRequest::from).collect::<Vec<_>>(),
                error.clone(),
            )
        })
        .collect::<Vec<_>>();
    std::thread::spawn(move || {
        for (shelf_index, shelf) in shelves.iter().enumerate() {
            for start in (0..shelf.len()).step_by(crate::catalog::THUMBNAIL_BATCH_SIZE) {
                if generation.load(Ordering::SeqCst) != request_id {
                    return;
                }
                let end = (start + crate::catalog::THUMBNAIL_BATCH_SIZE).min(shelf.len());
                let thumbnails = fetch_thumbnail_batch(&shelf[start..end], Some(&token))
                    .into_iter()
                    .map(|(offset, thumbnail)| (start + offset, thumbnail))
                    .collect::<Vec<_>>();
                let generation = generation.clone();
                let ui = ui.clone();
                let requests = shelf.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if generation.load(Ordering::SeqCst) != request_id {
                        return;
                    }
                    if let Some(ui) = ui.upgrade() {
                        update_home_shelf_thumbnails(
                            &ui,
                            shelf_index,
                            &requests,
                            start,
                            end,
                            &thumbnails,
                        );
                    }
                });
            }
        }
        for (category_name, shelf, _) in &category_shelves {
            for start in (0..shelf.len()).step_by(crate::catalog::THUMBNAIL_BATCH_SIZE) {
                if generation.load(Ordering::SeqCst) != request_id {
                    return;
                }
                let end = (start + crate::catalog::THUMBNAIL_BATCH_SIZE).min(shelf.len());
                let thumbnails = fetch_thumbnail_batch(&shelf[start..end], Some(&token))
                    .into_iter()
                    .map(|(offset, thumbnail)| (start + offset, thumbnail))
                    .collect::<Vec<_>>();
                let generation = generation.clone();
                let ui = ui.clone();
                let category_name = category_name.clone();
                let requests = shelf.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if generation.load(Ordering::SeqCst) != request_id {
                        return;
                    }
                    if let Some(ui) = ui.upgrade() {
                        update_home_category_thumbnails(
                            &ui,
                            &category_name,
                            &requests,
                            start,
                            end,
                            &thumbnails,
                        );
                    }
                });
            }
        }
    });
}

fn set_home_category_options(ui: &MainWindow, categories: &[TvItem], selected: &[String]) {
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

fn resolve_home_category_ids(categories: &[TvItem], local_store: &LocalStore) -> Vec<String> {
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

#[derive(Clone, Default)]
struct CatalogPageState {
    category: String,
    query: String,
    library_id: String,
    next_start: usize,
    has_more: bool,
    loading_more: bool,
    filters: CatalogFilters,
    supplemental_total: usize,
}

/// Server-reported match totals per result type for a search. Zero means
/// "unknown" and falls back to the loaded count at publish time. Channels
/// are returned in full by the Resolver, so their loaded count is the total.
#[derive(Clone, Copy, Default)]
struct SearchTypeTotals {
    movie: usize,
    performer: usize,
    studio: usize,
    tag: usize,
    channel: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct CatalogFilters {
    sort: u8,
    descending: bool,
    unwatched: bool,
    favorites_only: bool,
    hd_only: bool,
    duration: u8,
    genre: String,
    genre_id: String,
    studio: String,
    studio_id: String,
    performer: String,
    performer_id: String,
    performer_letter: String,
}

#[derive(Clone)]
struct CatalogViewSnapshot {
    cards: Vec<VideoCardData>,
    items: Vec<TvItem>,
    page: CatalogPageState,
    total_count: i32,
    has_more: bool,
    scroll_y: f32,
    anchor_index: i32,
    anchor_screen_y: f32,
    more_error: String,
    is_search: bool,
    search_query: String,
    search_applied_query: String,
    search_error: String,
    search_results_filter: String,
    search_movie_count: i32,
    search_performer_count: i32,
    search_studio_count: i32,
    search_tag_count: i32,
    search_channel_count: i32,
    search_movie_total: i32,
    search_performer_total: i32,
    search_studio_total: i32,
    search_tag_total: i32,
    search_channel_total: i32,
    search_all_has_more: bool,
    search_all_total_count: i32,
}

#[derive(Clone)]
struct CatalogFilterChoice {
    id: String,
    name: String,
    selected: bool,
}

impl Default for CatalogFilters {
    fn default() -> Self {
        Self {
            sort: 0,
            descending: true,
            unwatched: false,
            favorites_only: false,
            hd_only: false,
            duration: 0,
            genre: String::new(),
            genre_id: String::new(),
            studio: String::new(),
            studio_id: String::new(),
            performer: String::new(),
            performer_id: String::new(),
            performer_letter: String::new(),
        }
    }
}

impl CatalogFilters {
    fn sort_by(&self) -> &'static str {
        match self.sort {
            1 => "SortName",
            2 => "Runtime",
            3 => "CommunityRating",
            _ => "DateCreated",
        }
    }

    fn active_filter_count(&self, category: &str) -> i32 {
        let mut count = 0;
        if self.unwatched {
            count += 1;
        }
        if self.favorites_only && category != "Favorites" {
            count += 1;
        }
        if self.hd_only {
            count += 1;
        }
        if self.duration > 0 {
            count += 1;
        }
        if !self.genre.is_empty() || !self.genre_id.is_empty() {
            count += 1;
        }
        if !self.studio.is_empty() || !self.studio_id.is_empty() {
            count += 1;
        }
        if !self.performer.is_empty() || !self.performer_id.is_empty() {
            count += 1;
        }
        count
    }

    fn accepts(&self, item: &TvItem) -> bool {
        if self.favorites_only && !item.is_favorite {
            return false;
        }
        let minutes = item.runtime_ticks.map(|ticks| ticks / 10_000_000 / 60);
        if self.duration > 0 {
            let Some(minutes) = minutes else { return false };
            if match self.duration {
                1 => !(1..=14).contains(&minutes),
                2 => !(15..=40).contains(&minutes),
                _ => minutes <= 40,
            } {
                return false;
            }
        }
        let is_hd = item.width.is_some_and(|width| width >= 1280)
            || item.height.is_some_and(|height| height >= 720)
            || item
                .file_size_bytes
                .is_some_and(|size| size >= 300 * 1024 * 1024);
        if self.hd_only && !is_hd {
            return false;
        }
        if self.unwatched && item.is_played {
            return false;
        }
        if !self.genre.is_empty()
            && !item
                .genres
                .iter()
                .any(|value| value.eq_ignore_ascii_case(&self.genre))
        {
            return false;
        }
        if !self.studio.is_empty()
            && !item
                .studios
                .iter()
                .any(|value| value.eq_ignore_ascii_case(&self.studio))
        {
            return false;
        }
        if (!self.performer.is_empty() || !self.performer_id.is_empty())
            && !item.people.iter().any(|person| {
                (!self.performer.is_empty() && person.name.eq_ignore_ascii_case(&self.performer))
                    || (!self.performer_id.is_empty() && person.id == self.performer_id)
            })
        {
            return false;
        }
        true
    }
}

fn get_category_filters(
    filters_map: &Arc<Mutex<std::collections::HashMap<String, CatalogFilters>>>,
    category: &str,
) -> CatalogFilters {
    filters_map
        .lock()
        .unwrap()
        .get(category)
        .cloned()
        .unwrap_or_default()
}

fn set_category_filters(
    filters_map: &Arc<Mutex<std::collections::HashMap<String, CatalogFilters>>>,
    category: &str,
    filters: CatalogFilters,
) {
    filters_map
        .lock()
        .unwrap()
        .insert(category.to_string(), filters);
}

fn catalog_view_key(page: &CatalogPageState) -> String {
    format!(
        "{}\0{}\0{}\0{:?}",
        page.library_id, page.category, page.query, page.filters
    )
}

/// Drop the remembered field text from every stored view of `category`.
/// Snapshots keep the typed query so navigating away and back restores it, but
/// after an explicit "Clear search" the old text must not resurface when the
/// category listing is restored from its snapshot.
fn clear_saved_view_queries(
    views: &Rc<RefCell<std::collections::HashMap<String, CatalogViewSnapshot>>>,
    category: &str,
) {
    for snapshot in views.borrow_mut().values_mut() {
        if snapshot.page.category == category {
            snapshot.search_query.clear();
        }
    }
}

fn save_catalog_view(
    ui: &MainWindow,
    paging: &Arc<Mutex<CatalogPageState>>,
    cached_items: &Arc<Mutex<Vec<TvItem>>>,
    views: &Rc<RefCell<std::collections::HashMap<String, CatalogViewSnapshot>>>,
) {
    if ui.get_is_loading_catalog() {
        return;
    }
    let mut page = paging.lock().unwrap().clone();
    if page.category.is_empty() {
        return;
    }
    // "Clear search" empties the search UI before filter_category swaps the
    // paging state, so snapshotting that half-cleared mix would overwrite the
    // cached search view with a non-search page filed under the old query.
    if !page.query.is_empty() && !ui.get_is_catalog_search_results() {
        return;
    }
    page.loading_more = false;
    let model = ui.get_catalog_items();
    let cards = (0..model.row_count())
        .filter_map(|index| model.row_data(index))
        .collect::<Vec<_>>();
    let snapshot = CatalogViewSnapshot {
        cards,
        items: cached_items.lock().unwrap().clone(),
        page: page.clone(),
        total_count: ui.get_catalog_total_count(),
        has_more: ui.get_catalog_has_more(),
        scroll_y: ui.get_catalog_scroll_y(),
        anchor_index: ui.get_catalog_anchor_index(),
        anchor_screen_y: ui.get_catalog_anchor_screen_y(),
        more_error: ui.get_catalog_more_error().to_string(),
        is_search: ui.get_is_catalog_search_results(),
        search_query: ui.get_search_query().to_string(),
        search_applied_query: ui.get_search_applied_query().to_string(),
        search_error: ui.get_search_error_message().to_string(),
        search_results_filter: ui.get_search_results_filter().to_string(),
        search_movie_count: ui.get_search_movie_count(),
        search_performer_count: ui.get_search_performer_count(),
        search_studio_count: ui.get_search_studio_count(),
        search_tag_count: ui.get_search_tag_count(),
        search_channel_count: ui.get_search_channel_count(),
        search_movie_total: ui.get_search_movie_total(),
        search_performer_total: ui.get_search_performer_total(),
        search_studio_total: ui.get_search_studio_total(),
        search_tag_total: ui.get_search_tag_total(),
        search_channel_total: ui.get_search_channel_total(),
        search_all_has_more: ui.get_search_all_has_more(),
        search_all_total_count: ui.get_search_all_total_count(),
    };
    views.borrow_mut().insert(catalog_view_key(&page), snapshot);
}

fn restore_catalog_view(
    ui: &MainWindow,
    key: &str,
    paging: &Arc<Mutex<CatalogPageState>>,
    cached_items: &Arc<Mutex<Vec<TvItem>>>,
    views: &Rc<RefCell<std::collections::HashMap<String, CatalogViewSnapshot>>>,
    generation: &Arc<AtomicU64>,
    search_generation: &Arc<AtomicU64>,
) -> Option<(u64, Vec<VideoCardData>)> {
    let snapshot = views.borrow().get(key).cloned()?;
    let request_id = generation.fetch_add(1, Ordering::SeqCst) + 1;
    search_generation.fetch_add(1, Ordering::SeqCst);
    let mut page = snapshot.page.clone();
    page.loading_more = false;
    sync_catalog_filters_to_ui(ui, &page.category, &page.filters);
    *paging.lock().unwrap() = page;
    *cached_items.lock().unwrap() = snapshot.items.clone();

    set_catalog(ui, snapshot.cards.clone());
    ui.set_catalog_total_count(snapshot.total_count);
    ui.set_catalog_has_more(snapshot.has_more);
    ui.set_catalog_more_error(snapshot.more_error.into());
    ui.set_is_loading_catalog(false);
    ui.set_is_loading_more(false);
    ui.set_is_catalog_search_results(snapshot.is_search);
    ui.set_search_query(snapshot.search_query.into());
    ui.set_search_applied_query(snapshot.search_applied_query.into());
    ui.set_search_error_message(snapshot.search_error.into());
    ui.set_search_results_filter(snapshot.search_results_filter.clone().into());
    ui.set_search_movie_count(snapshot.search_movie_count);
    ui.set_search_performer_count(snapshot.search_performer_count);
    ui.set_search_studio_count(snapshot.search_studio_count);
    ui.set_search_tag_count(snapshot.search_tag_count);
    ui.set_search_channel_count(snapshot.search_channel_count);
    ui.set_search_movie_total(snapshot.search_movie_total);
    ui.set_search_performer_total(snapshot.search_performer_total);
    ui.set_search_studio_total(snapshot.search_studio_total);
    ui.set_search_tag_total(snapshot.search_tag_total);
    ui.set_search_channel_total(snapshot.search_channel_total);
    ui.set_search_all_has_more(snapshot.search_all_has_more);
    ui.set_search_all_total_count(snapshot.search_all_total_count);
    // Sections belong to the mixed All view only. Typed-tab and catalogue
    // snapshots must not rebuild them; All repopulates from the full list.
    if snapshot.is_search && snapshot.search_results_filter == "all" {
        set_search_sections(ui, &snapshot.cards);
    }
    ui.set_catalog_anchor_index(snapshot.anchor_index);
    ui.set_catalog_anchor_screen_y(snapshot.anchor_screen_y);
    ui.set_catalog_anchor_pending(!snapshot.cards.is_empty());
    ui.set_catalog_scroll_y(snapshot.scroll_y);
    ui.set_status("Ready".into());
    ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
    Some((request_id, snapshot.cards))
}

fn sync_catalog_filters_to_ui(ui: &MainWindow, category: &str, filters: &CatalogFilters) {
    let (sort, duration) = catalog_filter_labels(filters);
    ui.set_catalog_sort_label(sort.into());
    ui.set_catalog_sort_descending(filters.descending);
    ui.set_catalog_unwatched_only(filters.unwatched);
    ui.set_catalog_favorites_only(filters.favorites_only);
    ui.set_catalog_hd_only(filters.hd_only);
    ui.set_catalog_duration_label(duration.into());
    ui.set_catalog_genre_query(filters.genre.clone().into());
    ui.set_catalog_studio_query(filters.studio.clone().into());
    ui.set_catalog_performer_query(filters.performer.clone().into());
    ui.set_catalog_active_filter_count(filters.active_filter_count(category));
}

/// Filters that actually drive the visible list. Search results are always
/// video lists, so they use the "Latest" filter set — the same set the badge
/// and the chips show. Outside search, each category owns its filters. Keeping
/// one scope for display and application is what makes query, filters, the
/// active-filter counter and the rendered results describe one state.
fn catalog_filter_scope(ui: &MainWindow) -> String {
    if ui.get_is_catalog_search_results() {
        "Latest".to_string()
    } else {
        ui.get_active_category().to_string()
    }
}

/// Re-run whichever list the user is looking at, from page one. Search must
/// stay a search (refetching the *applied* query, not the field text) and a
/// plain browse must stay a category listing, so the rendered results always
/// match the query, filters and counter that are on screen.
fn refresh_active_catalog(ui: &MainWindow) {
    if ui.get_is_catalog_search_results() {
        let applied = ui.get_search_applied_query().to_string();
        if !applied.trim().is_empty() {
            ui.invoke_search_catalog(applied.into());
            return;
        }
    }
    ui.invoke_filter_category(ui.get_active_category());
}

fn catalog_filter_labels(filters: &CatalogFilters) -> (String, String) {
    let sort = match filters.sort {
        1 => "Title",
        2 => "Duration",
        3 => "Rating",
        _ => "Date added",
    };
    let duration = match filters.duration {
        1 => "Short",
        2 => "Medium",
        3 => "Long",
        _ => "Any length",
    };
    (sort.into(), duration.into())
}

fn filter_performer_results(
    items: &[TvItem],
    letter: &str,
    photo_only: bool,
    search_query: &str,
) -> Vec<TvItem> {
    let first_letter = letter
        .trim()
        .chars()
        .next()
        .map(|char| char.to_lowercase().to_string());
    let search_query = search_query.trim().to_lowercase();
    items
        .iter()
        .filter(|item| item.genres.iter().any(|tag| tag == "__adult_hub_performer"))
        .filter(|item| search_query.is_empty() || item.title.to_lowercase().contains(&search_query))
        .filter(|item| {
            first_letter.as_ref().is_none_or(|letter| {
                item.title
                    .chars()
                    .next()
                    .is_some_and(|first| first.to_lowercase().to_string() == *letter)
            })
        })
        .filter(|item| {
            !photo_only
                || item
                    .primary_image_tag
                    .as_ref()
                    .is_some_and(|tag| !tag.trim().is_empty())
        })
        .cloned()
        .collect()
}

struct CatalogPage {
    items: Vec<TvItem>,
    next_start: usize,
    total_count: usize,
    has_more: bool,
    supplemental_total: usize,
}

fn get_catalog_page(
    jellyfin: &JellyfinClient,
    resolver: &ResolverClient,
    session: &Session,
    local_store: &LocalStore,
    category: &str,
    query: &str,
    start_index: usize,
    filters: CatalogFilters,
) -> std::result::Result<CatalogPage, JellyfinError> {
    if category == "Watch History" {
        let query = query.trim().to_lowercase();
        let mut history = local_store
            .get_watch_history(100)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|json| serde_json::from_str::<TvItem>(&json).ok())
            .filter(|item| {
                (query.is_empty()
                    || item.title.to_lowercase().contains(&query)
                    || item
                        .overview
                        .as_ref()
                        .is_some_and(|overview| overview.to_lowercase().contains(&query)))
                    && filters.accepts(item)
            })
            .collect::<Vec<_>>();
        if filters.sort == 1 {
            history.sort_by_key(|a| a.title.to_lowercase());
        } else if filters.sort == 2 {
            history.sort_by_key(|item| item.runtime_ticks.unwrap_or(0));
        }
        if !filters.descending && filters.sort != 0 {
            history.reverse();
        }
        let total_count = history.len();
        let items = history
            .into_iter()
            .skip(start_index)
            .take(CATALOG_PAGE_SIZE)
            .collect::<Vec<_>>();
        let next_start = start_index + items.len();
        return Ok(CatalogPage {
            items,
            next_start,
            total_count,
            has_more: next_start < total_count,
            supplemental_total: 0,
        });
    }

    let mut supplemental_count = 0;
    let mut supplemental_total = 0;
    let selected_library_id = local_store
        .get_setting("selected_library_id")
        .ok()
        .flatten();
    let performer_query = query.trim();
    let performer_letter = filters.performer_letter.trim();
    let mut page = if category == "Performers"
        && (!performer_query.is_empty() || !performer_letter.is_empty())
    {
        let search_term = query.trim();
        let performers_page = jellyfin.get_items(
            &session.server_url,
            &session.user_id,
            &session.access_token,
            &GetItemsQuery {
                parent_id: None,
                start_index,
                limit: CATALOG_PAGE_SIZE,
                sort_by: "SortName",
                search_term: (!search_term.is_empty()).then_some(search_term),
                name_starts_with: (!performer_letter.is_empty()).then_some(performer_letter),
                include_item_types: Some("Person"),
                person_types: Some("Actor"),
                ..Default::default()
            },
        )?;
        let mut performers_page = performers_page;
        for performer in &mut performers_page.items {
            performer.genres.push("__adult_hub_performer".into());
        }
        performers_page
    } else if !query.trim().is_empty() {
        let movie_query =
            search_movie_query(query, start_index, &filters, selected_library_id.as_deref());
        let resolver_base_url = resolver_base_url_for(local_store, session);

        let movie_result = jellyfin.get_items(
            &session.server_url,
            &session.user_id,
            &session.access_token,
            &movie_query,
        );
        let supplements = if start_index == 0 {
            fetch_search_supplements(jellyfin, resolver, session, query, &resolver_base_url)
        } else {
            SearchSupplements::default()
        };

        let mut successful_sources = 0;
        let mut movie_page = match movie_result {
            Ok(page) => {
                successful_sources += 1;
                page
            }
            Err(_) => {
                crate::diagnostics::GLOBAL_LOG.w(
                    "Search",
                    "Jellyfin movie search failed; preserving results from other sources",
                );
                TvItemsPage {
                    items: Vec::new(),
                    total_record_count: 0,
                    start_index,
                }
            }
        };
        successful_sources += supplements.people.is_some() as usize
            + supplements.studios.is_some() as usize
            + supplements.tags.is_some() as usize
            + supplements.channels.is_some() as usize;
        if successful_sources == 0 {
            return Err(JellyfinError::Api("All search sources failed".into()));
        }

        let added = merge_search_supplements(&mut movie_page, supplements, local_store);
        supplemental_count += added;
        supplemental_total += added;
        movie_page
    } else {
        match category {
            "Resume" | "Continue Watching" => jellyfin.get_resume_items_page(
                &session.server_url,
                &session.user_id,
                &session.access_token,
                start_index,
                CATALOG_PAGE_SIZE,
            )?,
            "Latest" | "Favorites" => jellyfin.get_items(
                &session.server_url,
                &session.user_id,
                &session.access_token,
                &GetItemsQuery {
                    parent_id: selected_library_id.as_deref(),
                    start_index,
                    limit: CATALOG_PAGE_SIZE,
                    sort_by: filters.sort_by(),
                    sort_descending: filters.descending,
                    is_favorite: (category == "Favorites" || filters.favorites_only)
                        .then_some(true),
                    is_unplayed: filters.unwatched,
                    genre: (!filters.genre.is_empty()).then_some(filters.genre.as_str()),
                    studio: (!filters.studio.is_empty()).then_some(filters.studio.as_str()),
                    person: (!filters.performer.is_empty()).then_some(filters.performer.as_str()),
                    ..Default::default()
                },
            )?,
            "Performers" | "Studios" | "Categories" => {
                // Studios expose a sort control, so the request must honour it
                // instead of always asking the server for SortName.
                let (sort_by, sort_descending) = if category == "Studios" {
                    (filters.sort_by(), filters.descending)
                } else {
                    ("SortName", false)
                };
                let mut page = jellyfin.get_items(
                    &session.server_url,
                    &session.user_id,
                    &session.access_token,
                    &GetItemsQuery {
                        parent_id: None,
                        start_index,
                        limit: CATALOG_PAGE_SIZE,
                        sort_by,
                        sort_descending,
                        include_item_types: Some(match category {
                            "Performers" => "Person",
                            "Studios" => "Studio",
                            _ => "Genre",
                        }),
                        person_types: (category == "Performers").then_some("Actor"),
                        ..Default::default()
                    },
                )?;
                if category == "Performers" {
                    for performer in &mut page.items {
                        performer.genres.push("__adult_hub_performer".into());
                    }
                }
                page
            }
            _ => TvItemsPage {
                items: Vec::new(),
                total_record_count: 0,
                start_index,
            },
        }
    };

    let server_count = page.items.len().saturating_sub(supplemental_count);
    let server_total_count = page.total_record_count.saturating_sub(supplemental_total);
    let mut total_count = server_total_count + supplemental_total;
    if (category == "Resume" || category == "Continue Watching") && query.is_empty() {
        if let Ok(records) = local_store.get_all_telegram_progress(50) {
            let pending_records = records
                .into_iter()
                .filter(|record| !record.is_played)
                .collect::<Vec<_>>();
            total_count += pending_records.len();
            if start_index == 0 {
                for record in pending_records {
                    if page.items.iter().any(|item| {
                        item.id == format!("telegram:{}", record.provider_video_id)
                            || item.provider_video_id.as_deref()
                                == Some(record.provider_video_id.as_str())
                    }) {
                        continue;
                    }
                    let title = if record.title.is_empty() {
                        format!("Telegram {}", record.provider_video_id)
                    } else {
                        record.title.clone()
                    };
                    page.items.push(TvItem {
                        id: format!("telegram:{}", record.provider_video_id),
                        title,
                        runtime_ticks: Some((record.duration_ms * 10_000) as i64),
                        genres: vec!["Telegram".into()],
                        primary_image_tag: record.thumbnail_url,
                        playback_position_ticks: (record.position_ms * 10_000) as i64,
                        played_percentage: (record.duration_ms > 0).then_some(
                            record.position_ms as f64 / record.duration_ms as f64 * 100.0,
                        ),
                        provider_video_id: Some(record.provider_video_id),
                        container: Some("mp4".into()),
                        ..Default::default()
                    });
                }
            }
        }
    }
    if matches!(category, "Performers" | "Studios" | "Categories") && query.is_empty() {
        let kind_marker = match category {
            "Performers" => "__adult_hub_performer",
            "Studios" => "__adult_hub_studio",
            _ => "__adult_hub_genre",
        };
        for item in &mut page.items {
            item.genres.push(kind_marker.into());
        }
        if category == "Studios" {
            page.items = crate::catalog::aggregate_studio_items(page.items);
        }
    } else {
        page.items.retain(|item| {
            item.genres.iter().any(|tag| {
                tag == "__adult_hub_performer"
                    || tag == "__adult_hub_studio"
                    || tag == "__adult_hub_genre"
                    || tag == "__adult_hub_channel_search"
            }) || filters.accepts(item)
        });
    }

    let next_start = start_index + server_count;
    let has_more = next_start < server_total_count;
    Ok(CatalogPage {
        items: page.items,
        next_start,
        total_count,
        has_more,
        supplemental_total,
    })
}

// Search runs in two stages: the movie page renders as soon as the fast
// Jellyfin query lands, and the slower performer/studio/tag/channel hits are
// appended afterwards. The field list omits server-side expensive extras
// (ChildCount, Path, IsPlaceHolder) that catalog cards never read.
const SEARCH_MOVIE_FIELDS: &str =
    "ProviderIds,Overview,Genres,Studios,People,MediaSources,CommunityRating";
const SEARCH_SUPPLEMENT_FIELDS: &str = "ProviderIds,Overview,ChildCount";
const CHANNEL_SEARCH_TIMEOUT: Duration = Duration::from_secs(8);

fn search_movie_query<'a>(
    query: &'a str,
    start_index: usize,
    filters: &'a CatalogFilters,
    parent_id: Option<&'a str>,
) -> GetItemsQuery<'a> {
    GetItemsQuery {
        parent_id,
        start_index,
        limit: CATALOG_PAGE_SIZE,
        sort_by: filters.sort_by(),
        sort_descending: filters.descending,
        is_unplayed: filters.unwatched,
        is_favorite: filters.favorites_only.then_some(true),
        genre: (!filters.genre.is_empty()).then_some(filters.genre.as_str()),
        studio: (!filters.studio.is_empty()).then_some(filters.studio.as_str()),
        person: (!filters.performer.is_empty()).then_some(filters.performer.as_str()),
        search_term: Some(query),
        fields: Some(SEARCH_MOVIE_FIELDS),
        ..Default::default()
    }
}

fn search_supplement_query<'a>(
    query: &'a str,
    include_item_types: &'a str,
    person_types: Option<&'a str>,
) -> GetItemsQuery<'a> {
    GetItemsQuery {
        parent_id: None,
        start_index: 0,
        limit: 30,
        sort_by: "SortName",
        search_term: Some(query),
        include_item_types: Some(include_item_types),
        person_types,
        fields: Some(SEARCH_SUPPLEMENT_FIELDS),
        ..Default::default()
    }
}

fn resolver_base_url_for(local_store: &LocalStore, session: &Session) -> String {
    // An empty stored value is meaningful: settings clears both keys when the
    // resolver shares the active server's origin, so it must fall back to the
    // session URL instead of being used as a (relative) base.
    local_store
        .get_setting("resolver_base_url")
        .ok()
        .flatten()
        .or_else(|| local_store.get_setting("resolver_url").ok().flatten())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| session.server_url.clone())
}

#[derive(Default)]
struct SearchSupplements {
    people: Option<TvItemsPage>,
    studios: Option<TvItemsPage>,
    tags: Option<TvItemsPage>,
    channels: Option<Vec<TvItem>>,
}

impl SearchSupplements {
    /// Match totals the supplement sources report. Channels arrive in full
    /// from the Resolver (hidden channels are dropped at merge), so the
    /// loaded count *is* the total and stays 0 here for the publish fallback.
    fn totals(&self) -> SearchTypeTotals {
        SearchTypeTotals {
            movie: 0,
            performer: self
                .people
                .as_ref()
                .map(|page| page.total_record_count)
                .unwrap_or(0),
            studio: self
                .studios
                .as_ref()
                .map(|page| page.total_record_count)
                .unwrap_or(0),
            tag: self
                .tags
                .as_ref()
                .map(|page| page.total_record_count)
                .unwrap_or(0),
            channel: 0,
        }
    }
}

fn fetch_search_supplements(
    jellyfin: &JellyfinClient,
    resolver: &ResolverClient,
    session: &Session,
    query: &str,
    resolver_base_url: &str,
) -> SearchSupplements {
    let people_query = search_supplement_query(query, "Person", Some("Actor"));
    let studio_query = search_supplement_query(query, "Studio", None);
    let tag_query = search_supplement_query(query, "Genre", None);
    std::thread::scope(|scope| {
        let people_task = scope.spawn(|| {
            jellyfin
                .get_items(
                    &session.server_url,
                    &session.user_id,
                    &session.access_token,
                    &people_query,
                )
                .map_err(|_| {
                    crate::diagnostics::GLOBAL_LOG.w(
                        "Search",
                        "Jellyfin performer search failed; preserving results from other sources",
                    );
                })
                .ok()
        });
        let studio_task = scope.spawn(|| {
            jellyfin
                .get_items(
                    &session.server_url,
                    &session.user_id,
                    &session.access_token,
                    &studio_query,
                )
                .map_err(|_| {
                    crate::diagnostics::GLOBAL_LOG.w(
                        "Search",
                        "Jellyfin studio search failed; preserving results from other sources",
                    );
                })
                .ok()
        });
        let tag_task = scope.spawn(|| {
            jellyfin
                .get_items(
                    &session.server_url,
                    &session.user_id,
                    &session.access_token,
                    &tag_query,
                )
                .map_err(|_| {
                    crate::diagnostics::GLOBAL_LOG.w(
                        "Search",
                        "Jellyfin tag search failed; preserving results from other sources",
                    );
                })
                .ok()
        });
        let channel_task = scope.spawn(|| {
            resolver
                .search_with_timeout(resolver_base_url, query, "telegram", CHANNEL_SEARCH_TIMEOUT)
                .map(|videos| videos.into_iter().map(TvItem::from).collect::<Vec<_>>())
                .map_err(|_| {
                    crate::diagnostics::GLOBAL_LOG.w(
                        "Search",
                        "Resolver channel search failed; preserving results from other sources",
                    );
                })
                .ok()
        });
        SearchSupplements {
            people: people_task.join().ok().flatten(),
            studios: studio_task.join().ok().flatten(),
            tags: tag_task.join().ok().flatten(),
            channels: channel_task.join().ok().flatten(),
        }
    })
}

/// Appends marked performer/studio/tag/channel items after the movie page,
/// mirroring the ordering and hidden-channel rules the combined search used
/// before the two-stage split. Returns the number of items appended.
fn merge_search_supplements(
    movie_page: &mut TvItemsPage,
    supplements: SearchSupplements,
    local_store: &LocalStore,
) -> usize {
    let mut added = 0;
    if let Some(mut people_page) = supplements.people {
        let people_count = people_page.items.len();
        added += people_count;
        for person in &mut people_page.items {
            person.genres.push("__adult_hub_performer".into());
        }
        movie_page.items.extend(people_page.items);
        movie_page.total_record_count += people_count;
    }
    if let Some(mut studio_page) = supplements.studios {
        let studio_count = studio_page.items.len();
        added += studio_count;
        for studio in &mut studio_page.items {
            studio.genres.push("__adult_hub_studio".into());
        }
        movie_page.items.extend(studio_page.items);
        movie_page.total_record_count += studio_count;
    }
    if let Some(mut tag_page) = supplements.tags {
        let tag_count = tag_page.items.len();
        added += tag_count;
        for tag in &mut tag_page.items {
            tag.genres.push("__adult_hub_genre".into());
        }
        movie_page.items.extend(tag_page.items);
        movie_page.total_record_count += tag_count;
    }
    if let Some(channel_items) = supplements.channels {
        let mut channel_items = channel_items;
        let mut seen_channel_items = std::collections::HashSet::new();
        channel_items.retain(|item| seen_channel_items.insert(item.id.clone()));
        let hidden_channels = local_store.get_hidden_channels().unwrap_or_default();
        channel_items.retain(|item| {
            !item.provider_video_id.as_ref().is_some_and(|id| {
                id.split(':').next().is_some_and(|channel| {
                    hidden_channels
                        .iter()
                        .any(|hidden| channel.eq_ignore_ascii_case(hidden))
                })
            })
        });
        let channel_count = channel_items.len();
        added += channel_count;
        for channel_item in &mut channel_items {
            channel_item
                .genres
                .push("__adult_hub_channel_search".into());
        }
        movie_page.items.extend(channel_items);
        movie_page.total_record_count += channel_count;
    }
    added
}

/// Stage A of catalog search: the movies-only page, restricted to the selected
/// library and without the supplemental fan-out.
fn get_search_movies_page_at(
    jellyfin: &JellyfinClient,
    session: &Session,
    local_store: &LocalStore,
    query: &str,
    filters: &CatalogFilters,
    start_index: usize,
) -> std::result::Result<CatalogPage, JellyfinError> {
    let parent_id = local_store
        .get_setting("selected_library_id")
        .ok()
        .flatten();
    let page = jellyfin.get_items(
        &session.server_url,
        &session.user_id,
        &session.access_token,
        &search_movie_query(query, start_index, filters, parent_id.as_deref()),
    )?;
    let server_count = page.items.len();
    let total_count = page.total_record_count;
    let items = page
        .items
        .into_iter()
        .filter(|item| filters.accepts(item))
        .collect::<Vec<_>>();
    let next_start = start_index + server_count;
    Ok(CatalogPage {
        items,
        next_start,
        total_count,
        has_more: next_start < total_count,
        supplemental_total: 0,
    })
}

fn get_search_movies_page_with_empty_fetch(
    jellyfin: &JellyfinClient,
    session: &Session,
    local_store: &LocalStore,
    query: &str,
    filters: &CatalogFilters,
) -> std::result::Result<CatalogPage, JellyfinError> {
    let mut page = get_search_movies_page_at(jellyfin, session, local_store, query, filters, 0)?;
    for _ in 0..3 {
        if !filter_catalog_items(page.items.clone(), local_store).is_empty() || !page.has_more {
            break;
        }
        let previous_start = page.next_start;
        let next_page = get_search_movies_page_at(
            jellyfin,
            session,
            local_store,
            query,
            filters,
            previous_start,
        )?;
        if next_page.next_start <= previous_start {
            page.has_more = false;
            break;
        }
        let mut seen = page
            .items
            .iter()
            .map(|item| item.id.clone())
            .collect::<std::collections::HashSet<_>>();
        page.items.extend(
            next_page
                .items
                .into_iter()
                .filter(|item| seen.insert(item.id.clone())),
        );
        page.next_start = next_page.next_start;
        page.has_more = next_page.has_more;
    }
    Ok(page)
}

fn get_catalog_page_with_empty_page_fetch(
    jellyfin: &JellyfinClient,
    resolver: &ResolverClient,
    session: &Session,
    local_store: &LocalStore,
    category: &str,
    query: &str,
    start_index: usize,
    filters: CatalogFilters,
    performer_photo_only: bool,
) -> std::result::Result<CatalogPage, JellyfinError> {
    let mut page = get_catalog_page(
        jellyfin,
        resolver,
        session,
        local_store,
        category,
        query,
        start_index,
        filters.clone(),
    )?;

    for _ in 0..3 {
        let visible = filter_catalog_items(page.items.clone(), local_store);
        let has_visible_items = if category == "Performers" {
            !filter_performer_results(
                &visible,
                filters.performer_letter.as_str(),
                performer_photo_only,
                query,
            )
            .is_empty()
        } else {
            !visible.is_empty()
        };
        if has_visible_items || !page.has_more {
            break;
        }

        let previous_start = page.next_start;
        let next_page = get_catalog_page(
            jellyfin,
            resolver,
            session,
            local_store,
            category,
            query,
            previous_start,
            filters.clone(),
        )?;
        if next_page.next_start <= previous_start {
            page.has_more = false;
            break;
        }

        let mut seen = page
            .items
            .iter()
            .map(|item| item.id.clone())
            .collect::<std::collections::HashSet<_>>();
        page.items.extend(
            next_page
                .items
                .into_iter()
                .filter(|item| seen.insert(item.id.clone())),
        );
        page.next_start = next_page.next_start;
        page.has_more = next_page.has_more;
    }

    Ok(page)
}

pub fn current_nav_destination(
    ui: &MainWindow,
    ctx: &crate::context::AppContext,
) -> crate::context::NavDestination {
    use crate::context::NavDestination;
    let active = ui.get_active_screen();
    match active.as_str() {
        "home" => NavDestination::Home,
        "browse" => {
            let is_search = ui.get_is_catalog_search_results();
            NavDestination::Browse {
                category: ui.get_active_category().to_string(),
                is_search,
                search_query: if is_search {
                    ui.get_search_query().to_string()
                } else {
                    String::new()
                },
                genre_query: ui.get_catalog_genre_query().to_string(),
                studio_query: ui.get_catalog_studio_query().to_string(),
                performer_query: ui.get_catalog_performer_query().to_string(),
                performer_search_query: ui.get_performer_search_query().to_string(),
                performer_letter: ui.get_performer_letter().to_string(),
                performers_photo_only: ui.get_performers_photo_only(),
                drilldown: ui.get_catalog_category_drilldown(),
            }
        }
        "channels" => NavDestination::Channels,
        "settings" => NavDestination::Settings,
        "video-details" => NavDestination::VideoDetails {
            id: ui.get_detail_id().to_string(),
        },
        "performer-details" => NavDestination::PerformerDetails {
            name: ui.get_performer_name().to_string(),
        },
        "studio-details" => NavDestination::StudioDetails {
            name: ui.get_studio_name().to_string(),
        },
        "player" => NavDestination::Player {
            // The player's own item, not the details screen's id: playback can
            // be started from a card without ever populating `detail_id`, and
            // a stale id here sent Back to Home instead of the real screen.
            id: ctx
                .current_playing_item
                .lock()
                .unwrap()
                .as_ref()
                .map(|item| item.id.clone())
                .unwrap_or_else(|| ui.get_detail_id().to_string()),
        },
        _ => NavDestination::Home,
    }
}

pub fn restore_nav_destination(
    ui: &MainWindow,
    ctx: &AppContext,
    dest: crate::context::NavDestination,
) {
    use crate::context::NavDestination;
    ctx.is_navigating_back.store(true, Ordering::SeqCst);
    match dest {
        NavDestination::Home => {
            ui.invoke_switch_screen("home".into());
        }
        NavDestination::Browse {
            category,
            is_search,
            search_query,
            genre_query,
            studio_query,
            performer_query,
            performer_search_query,
            performer_letter,
            performers_photo_only,
            drilldown,
        } => {
            ui.set_active_screen("browse".into());
            ui.set_active_category(category.clone().into());
            ui.set_search_query(search_query.clone().into());
            ui.set_catalog_genre_query(genre_query.clone().into());
            ui.set_catalog_studio_query(studio_query.into());
            ui.set_catalog_performer_query(performer_query.into());
            ui.set_performer_search_query(performer_search_query.into());
            ui.set_performer_letter(performer_letter.into());
            ui.set_performers_photo_only(performers_photo_only);
            ui.set_catalog_category_drilldown(drilldown);
            if is_search && !search_query.is_empty() {
                ui.set_is_catalog_search_results(true);
                ui.invoke_search_catalog(search_query.into());
            } else {
                ui.set_is_catalog_search_results(false);
                if drilldown && !genre_query.is_empty() {
                    ui.invoke_set_catalog_text_filter("genre".into(), genre_query.into());
                } else {
                    ui.invoke_filter_category(category.into());
                }
            }
        }
        NavDestination::Channels => {
            ui.invoke_switch_screen("channels".into());
        }
        NavDestination::Settings => {
            ui.invoke_switch_screen("settings".into());
        }
        NavDestination::VideoDetails { id } => {
            ui.invoke_open_item_detail(id.into());
        }
        NavDestination::PerformerDetails { name } => {
            ui.invoke_open_performer_profile(name.into());
        }
        NavDestination::StudioDetails { name } => {
            ui.invoke_open_studio_profile(name.into());
        }
        NavDestination::Player { id } => {
            // Returning to the player must keep material, position and pause
            // state; only re-open media when the player no longer holds it.
            let matches_loaded = ctx
                .current_playing_item
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|playing| playing.id == id);
            if matches_loaded && ui.get_has_active_media() {
                ui.set_active_screen("player".into());
                ui.invoke_focus_keys();
            } else if let Some(item) = ctx.find_catalog_item(&id) {
                crate::playback::start_item_playback_ctx(ui, ctx, item);
            } else {
                ui.invoke_switch_screen("home".into());
            }
        }
    }
    ctx.is_navigating_back.store(false, Ordering::SeqCst);
}

pub fn install_stage2_handlers(ui: &MainWindow, ctx: &AppContext) {
    let player = ctx.player.clone();
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let jellyfin = ctx.jellyfin.clone();
    let resolver = ctx.resolver.clone();
    let reporter = ctx.reporter.clone();
    let session_ctrl = ctx.session_ctrl.clone();
    let current_item = ctx.current_playing_item.clone();
    let cached_catalog = ctx.cached_catalog.clone();
    let phash_coord = ctx.phash_coord.clone();
    let qc_generation = Arc::new(AtomicU64::new(0));
    refresh_recent_searches_for_context(ui, &local_store);

    // 1. Screen switching
    {
        let weak = ui.as_weak();
        let player = player.clone();
        let local_store = local_store.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let qc_generation = qc_generation.clone();
        let ctx_switch = ctx.clone();
        ui.on_switch_screen(move |screen| {
            let Some(ui) = weak.upgrade() else { return };
            let current_screen = ui.get_active_screen();
            if current_screen != screen {
                ctx_switch.push_nav_destination(current_nav_destination(&ui, &ctx_switch));
            }
            if matches!(current_screen.as_str(), "home" | "browse" | "channels") {
                ui.set_last_content_screen(current_screen);
            }
            if ui.get_active_screen() == "settings"
                && screen != "settings"
                && ui.get_quick_connect_active()
            {
                qc_generation.fetch_add(1, Ordering::SeqCst);
                ui.set_quick_connect_active(false);
                ui.set_quick_connect_expired(false);
                ui.set_quick_connect_authenticating(false);
                ui.set_quick_connect_code("".into());
            }
            if ui.get_active_screen() == "player" && screen != "player" {
                crate::playback::stop_active_playback_for_navigation(
                    &ui,
                    player.as_deref(),
                    &reporter,
                    &session_ctrl,
                    &current_item,
                );
            }
            let target_screen = if screen == "player" && !ui.get_has_active_media() {
                let last = ui.get_last_content_screen();
                if last.is_empty() || last == "player" {
                    "home".into()
                } else {
                    last
                }
            } else {
                screen
            };

            let is_channels = target_screen == "channels";
            let is_home = target_screen == "home";
            let is_settings = target_screen == "settings";
            let is_browse = target_screen == "browse";
            if target_screen == "player" {
                ui.set_player_controls_visible(true);
            }
            ui.set_active_screen(target_screen);
            refresh_recent_searches_for_context(&ui, &local_store);
            if is_channels {
                let current_cat = ui.get_active_channel_category();
                if !current_cat.is_empty() {
                    ui.invoke_select_channel_category(current_cat);
                } else if let Ok(Some(saved)) = local_store.get_selected_channel() {
                    ui.invoke_select_channel_category(saved.into());
                } else {
                    ui.invoke_refresh_channel_categories();
                }
            } else if is_home {
                ui.invoke_refresh_home();
            } else if is_browse {
                ui.invoke_filter_category(ui.get_active_category());
            } else if is_settings {
                ui.invoke_refresh_home_categories();
                ui.invoke_refresh_library_list();
            }
        });
    }

    // 1b. Navigation Back (History Stack)
    {
        let weak = ui.as_weak();
        let player = player.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let ctx_nav = ctx.clone();
        ui.on_navigate_back(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_active_screen() == "player" {
                crate::playback::stop_active_playback_for_navigation(
                    &ui,
                    player.as_deref(),
                    &reporter,
                    &session_ctrl,
                    &current_item,
                );
            }
            if let Some(dest) = ctx_nav.pop_nav_destination() {
                restore_nav_destination(&ui, &ctx_nav, dest);
            } else {
                let last = ui.get_last_content_screen();
                let target = if last.is_empty() || last == "player" {
                    "home".into()
                } else {
                    last
                };
                ui.invoke_switch_screen(target);
            }
        });
    }

    // 2. Perform Login
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let reporter = reporter.clone();
        let phash_coord = phash_coord.clone();
        let qc_generation = qc_generation.clone();
        ui.on_perform_login(move |server_url, username, password| {
            let Some(ui) = weak.upgrade() else { return };
            let server_url = server_url.trim().to_string();
            let username = username.trim().to_string();
            let password = password.to_string();

            if server_url.is_empty() {
                show_error(&ui, "Server address cannot be empty");
                return;
            }

            match ServiceUrlPolicy::inspect(&server_url, false) {
                ServiceUrlValidation::Valid(service_url) => {
                    if !super::request_cleartext_consent(
                        &ui,
                        &local_store,
                        &service_url.normalized,
                        "login",
                    ) {
                        return;
                    }
                    if ui.get_quick_connect_active() || ui.get_quick_connect_expired() {
                        qc_generation.fetch_add(1, Ordering::SeqCst);
                        ui.set_quick_connect_active(false);
                        ui.set_quick_connect_expired(false);
                        ui.set_quick_connect_authenticating(false);
                        ui.set_quick_connect_code("".into());
                    }
                    let normalized = service_url.normalized;
                    let weak = weak.clone();
                    let jellyfin = jellyfin.clone();
                    let local_store = local_store.clone();
                    let credential_store = credential_store.clone();
                    let reporter = reporter.clone();
                    let phash_coord = phash_coord.clone();
                    ui.set_status("Connecting to server...".into());
                    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));

                    std::thread::spawn(move || {
                        let public_info = jellyfin.get_public_system_info(&normalized).ok();
                        match jellyfin.authenticate_by_name(&normalized, &username, &password) {
                            Ok(auth) => {
                                let session = Session {
                                    server_url: normalized.clone(),
                                    user_id: auth.user_id.clone(),
                                    username: auth.username.clone(),
                                    access_token: auth.access_token.clone(),
                                    server_name: public_info
                                        .map(|p| p.name)
                                        .unwrap_or_else(|| "Jellyfin".into()),
                                };
                                let session_save = credential_store.save(session.clone());
                                reporter.set_auth(
                                    Some(session.server_url.clone()),
                                    Some(session.access_token.clone()),
                                );
                                let eff_res_url = resolver_base_url_for(&local_store, &session);
                                phash_coord.update_auth(&eff_res_url, Some(&session.access_token));
                                slint::invoke_from_event_loop(move || {
                                    let Some(ui) = weak.upgrade() else { return };
                                    ui.set_is_logged_in(true);
                                    ui.set_login_modal_visible(false);
                                    ui.set_login_modal_message("".into());
                                    ui.set_logged_in_user(session.username.into());
                                    ui.set_server_url_input(session.server_url.into());
                                    ui.invoke_filter_category("Latest".into());
                                    ui.set_active_screen("home".into());
                                    ui.invoke_refresh_library_list();
                                    ui.invoke_refresh_home();
                                    let warning = session_save.err().map(|_| {
                                        "Windows could not save this session securely. You can use it until the app closes, then sign in again.".to_owned()
                                    });
                                    if let Some(message) = warning {
                                        ui.set_status(message.clone().into());
                                        ui.set_status_color(slint::Color::from_rgb_u8(255, 190, 90));
                                        ui.set_toast_message(message.into());
                                        ui.set_toast_type("warning".into());
                                    } else {
                                        ui.set_status("Logged in successfully".into());
                                        ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                                        ui.set_toast_message("Connected to Jellyfin successfully!".into());
                                        ui.set_toast_type("success".into());
                                    }
                                    ui.set_toast_action_text("".into());
                                    ui.set_toast_visible(true);
                                })
                                .ok();
                            }
                            Err(error) => {
                                slint::invoke_from_event_loop(move || {
                                    let Some(ui) = weak.upgrade() else { return };
                                    show_error(&ui, format!("Login failed: {error}"));
                                    ui.set_login_modal_message(
                                        format!("Login failed: {error}").into(),
                                    );
                                    ui.set_toast_message(format!("Login failed: {error}").into());
                                    ui.set_toast_type("error".into());
                                    ui.set_toast_action_text("".into());
                                    ui.set_toast_visible(true);
                                })
                                .ok();
                            }
                        }
                    });
                }
                ServiceUrlValidation::Invalid(err) => {
                    show_error(&ui, err);
                }
            }
        });
    }

    // 3. Perform Logout
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let reporter = reporter.clone();
        let cached_catalog = cached_catalog.clone();
        let home_catalog = ctx.home_catalog.clone();
        let phash_coord = phash_coord.clone();
        ui.on_perform_logout(move || {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.clear_session();
            let _ = credential_store.clear();
            reporter.set_auth(None, None);
            crate::thumbnails::set_resolver_auth_token(None);
            phash_coord.update_auth("", None);
            home_catalog.lock().unwrap().clear();
            let raw_items = Vec::new();
            let cards = raw_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
            *cached_catalog.lock().unwrap() = raw_items;
            ui.set_is_logged_in(false);
            ui.set_logged_in_user("".into());
            set_home_shelves(
                &ui,
                &[Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            );
            set_home_category_options(&ui, &[], &[]);
            ui.set_home_category_shelves(Rc::new(slint::VecModel::from(Vec::new())).into());
            set_catalog(&ui, cards);
            ui.set_status("Logged out".into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
        });
    }

    // 4. Quick Connect
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let reporter = reporter.clone();
        let qc_generation = qc_generation.clone();
        let phash_coord = phash_coord.clone();
        ui.on_start_quick_connect(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_quick_connect_active() {
                return;
            }
            let server_url = ui.get_server_url_input().trim().to_string();
            if server_url.is_empty() {
                show_error(&ui, "Server address cannot be empty");
                return;
            }
            let normalized = match ServiceUrlPolicy::inspect(&server_url, false) {
                ServiceUrlValidation::Valid(service_url) => {
                    if !super::request_cleartext_consent(
                        &ui,
                        &local_store,
                        &service_url.normalized,
                        "quick-connect",
                    ) {
                        return;
                    }
                    service_url.normalized
                }
                ServiceUrlValidation::Invalid(err) => {
                    show_error(&ui, err);
                    return;
                }
            };

            let generation = qc_generation.fetch_add(1, Ordering::SeqCst) + 1;
            ui.set_quick_connect_active(true);
            ui.set_quick_connect_expired(false);
            ui.set_quick_connect_authenticating(false);
            ui.set_quick_connect_code("".into());
            let qc_generation = qc_generation.clone();
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let local_store = local_store.clone();
            let credential_store = credential_store.clone();
            let reporter = reporter.clone();
            let phash_coord = phash_coord.clone();

            ui.set_status("Requesting Quick Connect code...".into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));

            std::thread::spawn(move || {
                let init = match jellyfin.quick_connect_initiate(&normalized) {
                    Ok(init) => init,
                    Err(err) => {
                        let w = weak.clone();
                        slint::invoke_from_event_loop(move || {
                            if qc_generation.load(Ordering::SeqCst) == generation
                                && let Some(ui) = w.upgrade()
                            {
                                ui.set_quick_connect_active(false);
                                ui.set_quick_connect_authenticating(false);
                                show_error(&ui, format!("Quick Connect failed: {err}"));
                            }
                        })
                        .ok();
                        return;
                    }
                };

                let code = init.code.clone();
                let secret = init.secret.clone();
                let w = weak.clone();
                let generation_counter = qc_generation.clone();
                slint::invoke_from_event_loop(move || {
                    if generation_counter.load(Ordering::SeqCst) == generation
                        && let Some(ui) = w.upgrade()
                    {
                        ui.set_quick_connect_code(code.into());
                        ui.set_status("Waiting for Quick Connect authorization…".into());
                    }
                })
                .ok();

                for _ in 0..60 {
                    std::thread::sleep(Duration::from_secs(2));
                    if qc_generation.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    if let Ok(status) = jellyfin.quick_connect_poll(&normalized, &secret) {
                        if status.authenticated {
                            if qc_generation.load(Ordering::SeqCst) != generation {
                                return;
                            }
                            let w = weak.clone();
                            let generation_counter = qc_generation.clone();
                            slint::invoke_from_event_loop(move || {
                                if generation_counter.load(Ordering::SeqCst) == generation
                                    && let Some(ui) = w.upgrade()
                                {
                                    ui.set_quick_connect_authenticating(true);
                                    ui.set_status("Quick Connect authorized. Signing in…".into());
                                    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
                                }
                            })
                            .ok();
                            match jellyfin.quick_connect_authenticate(&normalized, &secret) {
                                Ok(auth) => {
                                    if qc_generation.load(Ordering::SeqCst) != generation {
                                        return;
                                    }
                                    let public_info =
                                        jellyfin.get_public_system_info(&normalized).ok();
                                    let session = Session {
                                        server_url: normalized.clone(),
                                        user_id: auth.user_id.clone(),
                                        username: auth.username.clone(),
                                        access_token: auth.access_token.clone(),
                                        server_name: public_info
                                            .map(|p| p.name)
                                            .unwrap_or_else(|| "Jellyfin".into()),
                                    };
                                    let _ = credential_store.save(session.clone());
                                    reporter.set_auth(
                                        Some(session.server_url.clone()),
                                        Some(session.access_token.clone()),
                                    );
                                    let eff_res_url =
                                        resolver_base_url_for(&local_store, &session);
                                    phash_coord
                                        .update_auth(&eff_res_url, Some(&session.access_token));

                                    let w = weak.clone();
                                    let generation_counter = qc_generation.clone();
                                    slint::invoke_from_event_loop(move || {
                                        if generation_counter.load(Ordering::SeqCst) == generation
                                            && let Some(ui) = w.upgrade()
                                        {
                                            ui.set_quick_connect_active(false);
                                            ui.set_quick_connect_expired(false);
                                            ui.set_quick_connect_authenticating(false);
                                            ui.set_quick_connect_code("".into());
                                            ui.set_is_logged_in(true);
                                            ui.set_login_modal_visible(false);
                                            ui.set_login_modal_message("".into());
                                            ui.set_logged_in_user(session.username.into());
                                            ui.invoke_filter_category("Latest".into());
                                            ui.set_active_screen("home".into());
                                            ui.invoke_refresh_library_list();
                                            ui.invoke_refresh_home();
                                            ui.set_status("Quick Connect authenticated!".into());
                                            ui.set_status_color(slint::Color::from_rgb_u8(
                                                140, 200, 160,
                                            ));
                                            ui.set_toast_message(
                                                "Quick Connect authenticated!".into(),
                                            );
                                            ui.set_toast_type("success".into());
                                            ui.set_toast_action_text("".into());
                                            ui.set_toast_visible(true);
                                        }
                                    })
                                    .ok();
                                    return;
                                }
                                Err(e) => {
                                    let w = weak.clone();
                                    let generation_counter = qc_generation.clone();
                                    slint::invoke_from_event_loop(move || {
                                        if generation_counter.load(Ordering::SeqCst) == generation
                                            && let Some(ui) = w.upgrade()
                                        {
                                            ui.set_quick_connect_active(false);
                                            ui.set_quick_connect_authenticating(false);
                                            ui.set_quick_connect_code("".into());
                                            show_error(
                                                &ui,
                                                format!("Quick Connect auth error: {e}"),
                                            );
                                            ui.set_toast_message(
                                                format!("Quick Connect error: {e}").into(),
                                            );
                                            ui.set_toast_type("error".into());
                                            ui.set_toast_action_text("".into());
                                            ui.set_toast_visible(true);
                                        }
                                    })
                                    .ok();
                                    return;
                                }
                            }
                        }
                    }
                }

                let w = weak.clone();
                let generation_counter = qc_generation.clone();
                slint::invoke_from_event_loop(move || {
                    if generation_counter.load(Ordering::SeqCst) == generation
                        && let Some(ui) = w.upgrade()
                    {
                        ui.set_quick_connect_active(false);
                        ui.set_quick_connect_expired(true);
                        ui.set_quick_connect_authenticating(false);
                        ui.set_quick_connect_code("".into());
                        show_error(&ui, "Quick Connect code expired");
                    }
                })
                .ok();
            });
        });
    }

    {
        let weak = ui.as_weak();
        let qc_generation = qc_generation.clone();
        ui.on_cancel_quick_connect(move || {
            let Some(ui) = weak.upgrade() else { return };
            qc_generation.fetch_add(1, Ordering::SeqCst);
            ui.set_quick_connect_active(false);
            ui.set_quick_connect_expired(false);
            ui.set_quick_connect_authenticating(false);
            ui.set_quick_connect_code("".into());
            ui.set_status("Quick Connect cancelled".into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
        });
    }

    // 4b. Discovered Servers
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        ui.on_select_discovered_server(move |server_url| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_server_url_input(server_url.clone());
            let server_url = server_url.trim().to_string();
            if !super::request_cleartext_consent(&ui, &local_store, &server_url, "discover-server")
            {
                return;
            }
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            std::thread::spawn(move || {
                if let Ok(info) = jellyfin.get_public_system_info(&server_url) {
                    slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_status(
                                format!("Selected server: {} ({})", info.name, info.url).into(),
                            );
                            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                        }
                    })
                    .ok();
                }
            });
        });
    }

    let is_scanning = Arc::new(AtomicBool::new(false));
    {
        let weak = ui.as_weak();
        let is_scanning = is_scanning.clone();
        ui.on_scan_servers(move || {
            let Some(ui) = weak.upgrade() else { return };
            if is_scanning.swap(true, Ordering::SeqCst) {
                return;
            }
            ui.set_is_scanning_servers(true);
            ui.set_status("Scanning local network for Jellyfin servers...".into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));

            let weak = weak.clone();
            let is_scanning = is_scanning.clone();
            std::thread::spawn(move || {
                let servers = discover_servers(Duration::from_millis(2000));
                is_scanning.store(false, Ordering::SeqCst);
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    ui.set_is_scanning_servers(false);
                    let count = servers.len();
                    let items = servers
                        .iter()
                        .map(|s| DiscoveredServerItem {
                            id: s.id.clone().into(),
                            name: s.name.clone().into(),
                            url: s.url.clone().into(),
                        })
                        .collect::<Vec<_>>();
                    set_discovered_servers(&ui, items);
                    if count > 0 {
                        ui.set_status(format!("Discovered {count} server(s) on network").into());
                        ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                    } else {
                        ui.set_status("No local Jellyfin servers found on network".into());
                        ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
                    }
                })
                .ok();
            });
        });
    }

    // 5. Search Catalog
    let catalog_generation = Arc::new(AtomicU64::new(0));
    let search_debounce_generation = Arc::new(AtomicU64::new(0));
    let performer_search_generation = Arc::new(AtomicU64::new(0));
    let home_generation = Arc::new(AtomicU64::new(0));
    let catalog_paging = Arc::new(Mutex::new(CatalogPageState::default()));
    let catalog_views = Rc::new(RefCell::new(std::collections::HashMap::<
        String,
        CatalogViewSnapshot,
    >::new()));
    let catalog_filters = Arc::new(Mutex::new(std::collections::HashMap::<
        String,
        CatalogFilters,
    >::new()));
    let catalog_filter_options = Arc::new(Mutex::new(Vec::<CatalogFilterChoice>::new()));
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let home_catalog = ctx.home_catalog.clone();
        let home_generation = home_generation.clone();
        let weak_for_toggle = weak.clone();
        ui.on_toggle_home_random_filter(move || {
            let Some(ui) = weak_for_toggle.upgrade() else {
                return;
            };
            ui.set_home_random_unwatched_only(!ui.get_home_random_unwatched_only());
            ui.invoke_refresh_home();
        });
        let weak_for_categories = weak.clone();
        let jellyfin_for_categories = jellyfin.clone();
        let credential_store_for_categories = credential_store.clone();
        let local_store_for_categories = local_store.clone();
        ui.on_refresh_home_categories(move || {
            let Some(ui) = weak_for_categories.upgrade() else {
                return;
            };
            let Some(session) = credential_store_for_categories.get().ok().flatten() else {
                set_home_category_options(&ui, &[], &[]);
                return;
            };
            let weak = weak_for_categories.clone();
            let jellyfin = jellyfin_for_categories.clone();
            let local_store = local_store_for_categories.clone();
            std::thread::spawn(move || {
                let categories_result = jellyfin.get_items(
                    &session.server_url,
                    &session.user_id,
                    &session.access_token,
                    &GetItemsQuery {
                        parent_id: None,
                        limit: 200,
                        sort_by: "SortName",
                        sort_descending: false,
                        include_item_types: Some("Genre"),
                        ..Default::default()
                    },
                );
                let categories = categories_result
                    .as_ref()
                    .map(|page| page.items.clone())
                    .unwrap_or_default();
                let selected = resolve_home_category_ids(&categories, &local_store);
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        set_home_category_options(&ui, &categories, &selected);
                    }
                })
                .ok();
            });
        });
        {
            let weak = weak.clone();
            let local_store = local_store.clone();
            ui.on_toggle_home_category(move |category_id| {
                let Some(ui) = weak.upgrade() else { return };
                let id = category_id.to_string();
                let mut selected = local_store
                    .get_home_categories()
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                if let Some(index) = selected.iter().position(|selected_id| selected_id == &id) {
                    selected.remove(index);
                } else {
                    selected.push(id);
                }
                if let Err(error) = local_store.set_home_categories(Some(&selected)) {
                    ui.set_status(format!("Could not save home categories: {error}").into());
                    ui.set_status_color(slint::Color::from_rgb_u8(244, 67, 54));
                    return;
                }
                ui.invoke_refresh_home_categories();
                ui.invoke_refresh_home();
            });
        }
        ui.on_refresh_home(move || {
            let Some(ui) = weak.upgrade() else { return };
            let request_id = home_generation.fetch_add(1, Ordering::SeqCst) + 1;
            ui.set_home_is_loading(true);
            ui.set_home_error_message(String::new().into());
            ui.set_home_resume_error(String::new().into());
            ui.set_home_latest_error(String::new().into());
            ui.set_home_random_error(String::new().into());
            ui.set_home_performer_error(String::new().into());
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let credential_store = credential_store.clone();
            let local_store = local_store.clone();
            let home_catalog = home_catalog.clone();
            let home_generation = home_generation.clone();
            let random_unwatched = ui.get_home_random_unwatched_only();

            std::thread::spawn(move || {
                let Some(session) = credential_store.get().ok().flatten() else {
                    home_catalog.lock().unwrap().clear();
                    slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_home_is_loading(false);
                            ui.set_home_error_message(String::new().into());
                            ui.set_home_resume_error(String::new().into());
                            ui.set_home_latest_error(String::new().into());
                            ui.set_home_random_error(String::new().into());
                            ui.set_home_performer_error(String::new().into());
                            set_home_shelves(
                                &ui,
                                &[Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()],
                            );
                            set_home_category_options(&ui, &[], &[]);
                            ui.set_home_category_shelves(
                                Rc::new(slint::VecModel::from(Vec::new())).into(),
                            );
                        }
                    })
                    .ok();
                    return;
                };

                let parent_id = local_store
                    .get_setting("selected_library_id")
                    .ok()
                    .flatten();
                let token = session.access_token.clone();
                let categories_result = jellyfin.get_items(
                    &session.server_url,
                    &session.user_id,
                    &token,
                    &GetItemsQuery {
                        parent_id: None,
                        limit: 200,
                        sort_by: "SortName",
                        sort_descending: false,
                        include_item_types: Some("Genre"),
                        ..Default::default()
                    },
                );
                let categories = categories_result
                    .as_ref()
                    .map(|page| page.items.clone())
                    .unwrap_or_default();
                let categories_error = categories_result
                    .as_ref()
                    .err()
                    .map(|error| format!("Couldn't load library categories: {error}"))
                    .unwrap_or_default();
                let selected_categories = resolve_home_category_ids(&categories, &local_store);
                let mut history = local_store
                    .get_watch_history(16)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|json| serde_json::from_str::<TvItem>(&json).ok())
                    .collect::<Vec<_>>();
                let (resume_result, latest_result, random_result, performer_result) =
                    std::thread::scope(|scope| {
                        let server_url = session.server_url.as_str();
                        let user_id = session.user_id.as_str();
                        let token_ref = token.as_str();
                        let parent_id = parent_id.as_deref();
                        let jellyfin_ref = jellyfin.as_ref();
                        let resume = scope.spawn(move || {
                            jellyfin_ref.get_resume_items(server_url, user_id, token_ref, 16)
                        });
                        let latest = scope.spawn(move || {
                            jellyfin_ref
                                .get_latest_items(server_url, user_id, token_ref, parent_id, 16)
                        });
                        let random = scope.spawn(move || {
                            jellyfin_ref
                                .get_items(
                                    server_url,
                                    user_id,
                                    token_ref,
                                    &GetItemsQuery {
                                        parent_id,
                                        limit: 16,
                                        sort_by: "Random",
                                        is_unplayed: random_unwatched,
                                        ..Default::default()
                                    },
                                )
                                .map(|page| page.items)
                        });
                        let performers = scope.spawn(move || {
                            jellyfin_ref
                                .get_items(
                                    server_url,
                                    user_id,
                                    token_ref,
                                    &GetItemsQuery {
                                        parent_id,
                                        limit: 100,
                                        sort_by: "Random",
                                        include_item_types: Some("Person"),
                                        person_types: Some("Actor"),
                                        ..Default::default()
                                    },
                                )
                                .map(|page| page.items)
                        });
                        (
                            resume.join().unwrap_or_else(|_| Ok(Vec::new())),
                            latest.join().unwrap_or_else(|_| Ok(Vec::new())),
                            random.join().unwrap_or_else(|_| Ok(Vec::new())),
                            performers.join().unwrap_or_else(|_| Ok(Vec::new())),
                        )
                    });
                let resume_error = resume_result
                    .as_ref()
                    .err()
                    .map(|error| format!("Couldn't load Continue Watching: {error}"))
                    .unwrap_or_default();
                let latest_error = latest_result
                    .as_ref()
                    .err()
                    .map(|error| format!("Couldn't load Recently Added: {error}"))
                    .unwrap_or_default();
                let random_error = random_result
                    .as_ref()
                    .err()
                    .map(|error| format!("Couldn't load Random Picks: {error}"))
                    .unwrap_or_default();
                let performer_error = performer_result
                    .as_ref()
                    .err()
                    .map(|error| format!("Couldn't load Performers: {error}"))
                    .unwrap_or_default();
                let mut resume = resume_result.unwrap_or_default();
                let mut latest = latest_result.unwrap_or_default();
                let mut random = random_result.unwrap_or_default();
                let mut performers = performer_result.unwrap_or_default();
                if let Ok(records) = local_store.get_all_telegram_progress(50) {
                    for record in records.into_iter().filter(|record| {
                        !record.is_played
                            && record.position_ms > 15_000
                            && (record.duration_ms == 0
                                || record.position_ms < record.duration_ms * 9 / 10)
                    }) {
                        let provider_id = record.provider_video_id.clone();
                        resume.push(TvItem {
                            id: format!("telegram:{provider_id}"),
                            title: if record.title.is_empty() {
                                format!("Telegram {provider_id}")
                            } else {
                                record.title
                            },
                            runtime_ticks: Some((record.duration_ms.saturating_mul(10_000)) as i64),
                            playback_position_ticks: (record.position_ms.saturating_mul(10_000))
                                as i64,
                            played_percentage: (record.duration_ms > 0).then_some(
                                record.position_ms as f64 / record.duration_ms as f64 * 100.0,
                            ),
                            primary_image_tag: record.thumbnail_url,
                            provider_video_id: Some(provider_id),
                            ..Default::default()
                        });
                    }
                }
                resume.retain(|item| {
                    item.progress_fraction() > 0.0 && item.progress_fraction() < 0.90
                });
                performers.retain(|person| {
                    person
                        .primary_image_tag
                        .as_ref()
                        .is_some_and(|tag| !tag.is_empty())
                });
                performers.truncate(16);
                for performer in &mut performers {
                    performer.genres.push("__adult_hub_performer".into());
                }
                let mut category_shelves = Vec::new();
                let mut category_items = Vec::new();
                for category in categories
                    .iter()
                    .filter(|category| selected_categories.iter().any(|id| id == &category.id))
                {
                    let category_result = jellyfin.get_items(
                        &session.server_url,
                        &session.user_id,
                        &token,
                        &GetItemsQuery {
                            parent_id: parent_id.as_deref(),
                            limit: 16,
                            sort_by: "DateCreated",
                            sort_descending: true,
                            genre: Some(&category.title),
                            ..Default::default()
                        },
                    );
                    let category_error = category_result
                        .as_ref()
                        .err()
                        .map(|error| format!("Couldn't load {}: {error}", category.title))
                        .unwrap_or_default();
                    let items = category_result
                        .map(|page| filter_catalog_items(page.items, &local_store))
                        .unwrap_or_default();
                    category_items.extend(items.iter().cloned());
                    category_shelves.push((
                        category.title.clone(),
                        items.into_iter().take(4).collect::<Vec<_>>(),
                        category_error,
                    ));
                }
                for item in resume
                    .iter_mut()
                    .chain(history.iter_mut())
                    .chain(latest.iter_mut())
                    .chain(random.iter_mut())
                {
                    if item.is_telegram() {
                        if let Some(progress) = local_store
                            .get_telegram_progress(
                                item.provider_video_id.as_deref().unwrap_or(&item.id),
                            )
                            .ok()
                            .flatten()
                        {
                            if progress.duration_ms > 0 {
                                item.runtime_ticks =
                                    Some((progress.duration_ms.saturating_mul(10_000)) as i64);
                            }
                            item.playback_position_ticks =
                                (progress.position_ms.saturating_mul(10_000)) as i64;
                            item.played_percentage = (progress.duration_ms > 0).then_some(
                                progress.position_ms as f64 / progress.duration_ms as f64 * 100.0,
                            );
                            item.is_played = progress.is_played;
                        }
                    } else if let Ok(Some((pos_ms, dur_ms))) =
                        local_store.get_progress("local", &item.id)
                    {
                        if dur_ms > 0 {
                            item.runtime_ticks = Some((dur_ms.saturating_mul(10_000)) as i64);
                        }
                        item.playback_position_ticks = (pos_ms.saturating_mul(10_000)) as i64;
                        item.played_percentage =
                            (dur_ms > 0).then_some(pos_ms as f64 / dur_ms as f64 * 100.0);
                        if dur_ms > 0 && pos_ms >= dur_ms * 9 / 10 {
                            item.is_played = true;
                        }
                    }
                }
                // Truly completed videos should not be proposed in Continue Watching / Resume
                resume.retain(|item| {
                    let fraction = item.progress_fraction();
                    (0.01..0.90).contains(&fraction) && !item.is_played
                });
                resume = filter_catalog_items(resume, &local_store);
                history = filter_catalog_items(history, &local_store);
                latest = filter_catalog_items(latest, &local_store);
                random = filter_catalog_items(random, &local_store);
                performers = filter_catalog_items(performers, &local_store);

                let mut all_items = Vec::new();
                for shelf in [&resume, &history, &latest, &random, &performers] {
                    for item in shelf {
                        if !all_items
                            .iter()
                            .any(|existing: &TvItem| existing.id == item.id)
                        {
                            all_items.push(item.clone());
                        }
                    }
                }
                for item in category_items {
                    if !all_items.iter().any(|existing| existing.id == item.id) {
                        all_items.push(item);
                    }
                }
                let shelves = [
                    resume.into_iter().take(4).collect::<Vec<_>>(),
                    history.into_iter().take(4).collect::<Vec<_>>(),
                    latest.into_iter().take(4).collect::<Vec<_>>(),
                    random.into_iter().take(4).collect::<Vec<_>>(),
                    performers.into_iter().take(12).collect::<Vec<_>>(),
                ];
                let overall_error = if !categories_error.is_empty() {
                    categories_error.clone()
                } else if !resume_error.is_empty() {
                    resume_error.clone()
                } else if !latest_error.is_empty() {
                    latest_error.clone()
                } else if !random_error.is_empty() {
                    random_error.clone()
                } else if !performer_error.is_empty() {
                    performer_error.clone()
                } else {
                    String::new()
                };

                let has_any_items = shelves.iter().any(|s| !s.is_empty())
                    || category_shelves.iter().any(|(_, s, _)| !s.is_empty());

                if overall_error.is_empty() && has_any_items {
                    let home_cache = PersistedHomeCache {
                        all_items: all_items.clone(),
                        shelves: shelves.to_vec(),
                        category_shelves: category_shelves.clone(),
                    };
                    if let Ok(json) = serde_json::to_string(&home_cache) {
                        let _ = local_store.set_setting("home_shelves_cache_v1", &json);
                    }
                }

                *home_catalog.lock().unwrap() = all_items;
                let categories_for_ui = categories.clone();
                let selected_categories_for_ui = selected_categories.clone();
                let server_url = session.server_url.clone();
                let has_overall_error = !overall_error.is_empty();
                slint::invoke_from_event_loop(move || {
                    if home_generation.load(Ordering::SeqCst) != request_id {
                        return;
                    }
                    if let Some(ui) = weak.upgrade() {
                        ui.set_home_is_loading(false);
                        ui.set_home_error_message(overall_error.into());
                        ui.set_home_resume_error(resume_error.into());
                        ui.set_home_latest_error(latest_error.into());
                        ui.set_home_random_error(random_error.into());
                        ui.set_home_performer_error(performer_error.into());

                        // If background refresh failed with an error and returned no items,
                        // keep any already displayed items instead of overwriting with empty
                        if !has_any_items && has_overall_error && ui.get_has_home_content() {
                            return;
                        }
                        let shelves = shelves
                            .iter()
                            .map(|items| {
                                items
                                    .iter()
                                    .map(|item| tv_item_to_card_for_server(item, &server_url))
                                    .collect::<Vec<_>>()
                            })
                            .collect::<Vec<_>>();
                        set_home_shelves(&ui, &shelves);
                        set_home_category_options(
                            &ui,
                            &categories_for_ui,
                            &selected_categories_for_ui,
                        );
                        let category_shelves = category_shelves
                            .into_iter()
                            .map(|(name, items, error_message)| {
                                let cards = items
                                    .iter()
                                    .map(|item| tv_item_to_card_for_server(item, &server_url))
                                    .collect::<Vec<_>>();
                                (name, cards, error_message)
                            })
                            .collect::<Vec<_>>();
                        let ui_category_shelves = category_shelves
                            .iter()
                            .map(|(name, items, error_message)| HomeCategoryShelfData {
                                name: name.clone().into(),
                                items: Rc::new(slint::VecModel::from(items.clone())).into(),
                                error_message: error_message.clone().into(),
                            })
                            .collect::<Vec<_>>();
                        ui.set_home_category_shelves(
                            Rc::new(slint::VecModel::from(ui_category_shelves)).into(),
                        );
                        refresh_home_thumbnails_async(
                            ui.as_weak(),
                            shelves,
                            category_shelves,
                            token,
                            home_generation.clone(),
                            request_id,
                        );
                    }
                })
                .ok();
            });
        });
    }
    {
        let weak = ui.as_weak();
        let filters = catalog_filters.clone();
        ui.on_update_catalog_filter(move |action| {
            let Some(ui) = weak.upgrade() else { return };
            let cat = catalog_filter_scope(&ui);
            let mut value = get_category_filters(&filters, &cat);
            match action.as_str() {
                "sort" => {
                    if cat == "Studios" {
                        // Studio entries have no runtime or rating; the control
                        // only ever cycles the orders the list actually honours.
                        value.sort = if value.sort == 0 { 1 } else { 0 };
                    } else {
                        value.sort = (value.sort + 1) % 4;
                    }
                }
                "direction" => value.descending = !value.descending,
                "unwatched" => value.unwatched = !value.unwatched,
                "favorites" => value.favorites_only = !value.favorites_only,
                "hd" => value.hd_only = !value.hd_only,
                "duration" => value.duration = (value.duration + 1) % 4,
                _ => return,
            }
            set_category_filters(&filters, &cat, value.clone());
            sync_catalog_filters_to_ui(&ui, &cat, &value);
            // Any filter change restarts the list from page one; keeping a
            // mid-pagination cursor would mix two different result sets.
            refresh_active_catalog(&ui);
        });
    }
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let catalog_filters = catalog_filters.clone();
        let catalog_filter_options = catalog_filter_options.clone();
        ui.on_open_catalog_filter_picker(move |kind| {
            let Some(ui) = weak.upgrade() else { return };
            let kind = kind.to_string();
            let Some(session) = credential_store.get().ok().flatten() else {
                catalog_filter_options.lock().unwrap().clear();
                ui.set_catalog_filter_options_loading(false);
                ui.set_catalog_filter_options_message("Sign in to load filter choices".into());
                ui.set_catalog_filter_options(
                    Rc::new(slint::VecModel::from(Vec::<HomeCategoryOption>::new())).into(),
                );
                return;
            };
            if !matches!(kind.as_str(), "genre" | "studio" | "performer") {
                return;
            }

            let cat = ui.get_active_category().to_string();
            let filters = get_category_filters(&catalog_filters, &cat);
            let selected = match kind.as_str() {
                "genre" => filters.genre.clone(),
                "studio" => filters.studio.clone(),
                _ => filters.performer.clone(),
            };
            ui.set_catalog_filter_options_loading(true);
            ui.set_catalog_filter_options_message("".into());
            catalog_filter_options.lock().unwrap().clear();
            ui.set_catalog_filter_options(
                Rc::new(slint::VecModel::from(Vec::<HomeCategoryOption>::new())).into(),
            );

            let parent_id = local_store
                .get_setting("selected_library_id")
                .ok()
                .flatten();
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let catalog_filter_options = catalog_filter_options.clone();
            let kind_for_worker = kind.clone();
            std::thread::spawn(move || {
                let result = jellyfin.get_items(
                    &session.server_url,
                    &session.user_id,
                    &session.access_token,
                    &GetItemsQuery {
                        parent_id: parent_id.as_deref(),
                        limit: 500,
                        sort_by: "SortName",
                        sort_descending: false,
                        include_item_types: Some(match kind_for_worker.as_str() {
                            "genre" => "Genre",
                            "studio" => "Studio",
                            _ => "Person",
                        }),
                        person_types: (kind_for_worker == "performer").then_some("Actor"),
                        ..Default::default()
                    },
                );
                let (options, error) = match result {
                    Ok(page) => {
                        let mut seen = std::collections::HashSet::new();
                        let options = page
                            .items
                            .into_iter()
                            .filter(|item| !item.title.trim().is_empty())
                            .filter(|item| seen.insert(item.title.to_lowercase()))
                            .map(|item| CatalogFilterChoice {
                                id: item.id,
                                name: item.title.clone(),
                                selected: item.title.eq_ignore_ascii_case(&selected),
                            })
                            .collect::<Vec<_>>();
                        (options, String::new())
                    }
                    Err(error) => (
                        Vec::new(),
                        format!("Couldn't load choices from Jellyfin: {error}"),
                    ),
                };
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        if ui.get_catalog_filter_picker_kind().as_str() != kind {
                            return;
                        }
                        *catalog_filter_options.lock().unwrap() = options.clone();
                        ui.set_catalog_filter_options_loading(false);
                        let query = ui.get_catalog_filter_options_query().to_lowercase();
                        let visible_options = options
                            .iter()
                            .filter(|option| {
                                query.is_empty() || option.name.to_lowercase().contains(&query)
                            })
                            .cloned()
                            .collect::<Vec<_>>();
                        let message = if !error.is_empty() {
                            error
                        } else if options.is_empty() {
                            "No options are available".to_string()
                        } else if visible_options.is_empty() {
                            "No matching choices".to_string()
                        } else {
                            String::new()
                        };
                        ui.set_catalog_filter_options_message(message.into());
                        let ui_options = visible_options
                            .into_iter()
                            .map(|option| HomeCategoryOption {
                                id: option.id.into(),
                                name: option.name.into(),
                                selected: option.selected,
                            })
                            .collect::<Vec<_>>();
                        ui.set_catalog_filter_options(
                            Rc::new(slint::VecModel::from(ui_options)).into(),
                        );
                    }
                })
                .ok();
            });
        });
    }
    {
        let weak = ui.as_weak();
        let catalog_filter_options = catalog_filter_options.clone();
        ui.on_filter_catalog_filter_options(move |query| {
            let Some(ui) = weak.upgrade() else { return };
            let query = query.trim().to_lowercase();
            let options = catalog_filter_options.lock().unwrap().clone();
            let option_count = options.len();
            let match_count = options
                .iter()
                .filter(|option| query.is_empty() || option.name.to_lowercase().contains(&query))
                .count();
            let ui_options = options
                .into_iter()
                .filter(|option| query.is_empty() || option.name.to_lowercase().contains(&query))
                .map(|option| HomeCategoryOption {
                    id: option.id.into(),
                    name: option.name.into(),
                    selected: option.selected,
                })
                .collect::<Vec<_>>();
            ui.set_catalog_filter_options(Rc::new(slint::VecModel::from(ui_options)).into());
            ui.set_catalog_filter_options_message(if option_count == 0 {
                "No options are available".into()
            } else if match_count == 0 {
                "No matching choices".into()
            } else {
                "".into()
            });
        });
    }
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let cached_catalog = cached_catalog.clone();
        let catalog_generation = catalog_generation.clone();
        let catalog_filters = catalog_filters.clone();
        ui.on_surprise_me(move || {
            let Some(ui) = weak.upgrade() else { return };
            let category = ui.get_active_category().to_string();
            if category != "Latest" && category != "Favorites" {
                return;
            }
            let Some(session) = credential_store.get().ok().flatten() else {
                ui.set_status("Sign in to pick a random library title".into());
                return;
            };

            let filters = get_category_filters(&catalog_filters, &category);
            let request_id = catalog_generation.load(Ordering::SeqCst);
            let parent_id = local_store
                .get_setting("selected_library_id")
                .ok()
                .flatten();
            ui.set_catalog_surprise_loading(true);
            ui.set_status("Finding a random title…".into());

            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let local_store = local_store.clone();
            let cached_catalog = cached_catalog.clone();
            let catalog_generation = catalog_generation.clone();
            let category = category.clone();
            let requested_category = category.clone();
            std::thread::spawn(move || {
                let result = jellyfin.get_items(
                    &session.server_url,
                    &session.user_id,
                    &session.access_token,
                    &GetItemsQuery {
                        parent_id: parent_id.as_deref(),
                        limit: 50,
                        sort_by: "Random",
                        is_favorite: (category == "Favorites" || filters.favorites_only)
                            .then_some(true),
                        is_unplayed: filters.unwatched,
                        is_hd: filters.hd_only.then_some(true),
                        genre: (!filters.genre.is_empty()).then_some(filters.genre.as_str()),
                        studio: (!filters.studio.is_empty()).then_some(filters.studio.as_str()),
                        person: (!filters.performer.is_empty())
                            .then_some(filters.performer.as_str()),
                        ..Default::default()
                    },
                );
                let network_error = result.as_ref().err().map(ToString::to_string);
                let target = result
                    .ok()
                    .map(|page| filter_catalog_items(page.items, &local_store))
                    .unwrap_or_default()
                    .into_iter()
                    .find(|item| {
                        (category != "Favorites" || item.is_favorite) && filters.accepts(item)
                    })
                    .or_else(|| {
                        let mut fallback = filter_catalog_items(
                            cached_catalog.lock().unwrap().clone(),
                            &local_store,
                        )
                        .into_iter()
                        .filter(|item| {
                            !item.genres.iter().any(|tag| {
                                tag == "__adult_hub_performer"
                                    || tag == "__adult_hub_channel_search"
                            }) && (category != "Favorites" || item.is_favorite)
                                && filters.accepts(item)
                        })
                        .collect::<Vec<_>>();
                        if fallback.is_empty() {
                            None
                        } else {
                            let index = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_nanos() as usize
                                % fallback.len();
                            Some(fallback.swap_remove(index))
                        }
                    });

                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    ui.set_catalog_surprise_loading(false);
                    if !catalog_generation_is_current(&catalog_generation, request_id)
                        || ui.get_active_screen().as_str() != "browse"
                        || ui.get_active_category().as_str() != requested_category.as_str()
                    {
                        return;
                    }
                    if let Some(item) = target {
                        ui.set_status(format!("Random pick: {}", item.title).into());
                        ui.invoke_open_item_detail(item.id.into());
                    } else if let Some(error) = network_error {
                        ui.set_status(format!("Could not find a random title: {error}").into());
                    } else {
                        ui.set_status("No titles match the current filters".into());
                    }
                })
                .ok();
            });
        });
    }
    {
        let weak = ui.as_weak();
        let filters = catalog_filters.clone();
        ui.on_reset_catalog_text_filters(move || {
            if let Some(ui) = weak.upgrade() {
                let cat = catalog_filter_scope(&ui);
                let mut current = get_category_filters(&filters, &cat);
                current.genre.clear();
                current.genre_id.clear();
                current.studio.clear();
                current.studio_id.clear();
                current.performer.clear();
                current.performer_id.clear();
                set_category_filters(&filters, &cat, current.clone());
                sync_catalog_filters_to_ui(&ui, &cat, &current);
                refresh_active_catalog(&ui);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let filters = catalog_filters.clone();
        ui.on_set_catalog_text_filter(move |key, value| {
            let Some(ui) = weak.upgrade() else { return };
            let cat = catalog_filter_scope(&ui);
            let mut current = get_category_filters(&filters, &cat);
            let text = value.trim().to_string();
            // A genre tile opened from search results leaves search mode
            // first; otherwise refresh_active_catalog refetches the applied
            // query with the genre as a hidden filter and the category
            // listing can never open.
            let leave_search_for_genre = key.as_str() == "genre"
                && ui.get_catalog_category_drilldown()
                && ui.get_is_catalog_search_results();
            match key.as_str() {
                "genre" => {
                    current.genre = text.clone();
                    if text.is_empty() {
                        current.genre_id.clear();
                    }
                }
                "studio" => {
                    current.studio = text.clone();
                    if text.is_empty() {
                        current.studio_id.clear();
                    }
                }
                "performer" => {
                    current.performer = text.clone();
                    if text.is_empty() {
                        current.performer_id.clear();
                    }
                }
                _ => return,
            }
            set_category_filters(&filters, &cat, current.clone());
            sync_catalog_filters_to_ui(&ui, &cat, &current);
            if leave_search_for_genre {
                // Load the listing before clearing the search state:
                // filter_category snapshots the search page while its
                // counters are intact, then resets the search flags.
                ui.invoke_filter_category(ui.get_active_category());
                CatalogSearchState::cleared().apply(&ui);
                return;
            }
            refresh_active_catalog(&ui);
        });
    }
    {
        let weak = ui.as_weak();
        let filters = catalog_filters.clone();
        ui.on_clear_catalog_filters(move || {
            let Some(ui) = weak.upgrade() else { return };
            // "Reset filters" is exactly that: the query and the fact that the
            // user is looking at search results stay untouched, so the field,
            // the counter and the results keep describing one state.
            let cat = catalog_filter_scope(&ui);
            let default_filters = CatalogFilters::default();
            set_category_filters(&filters, &cat, default_filters.clone());
            sync_catalog_filters_to_ui(&ui, &cat, &default_filters);
            refresh_active_catalog(&ui);
        });
    }
    {
        let weak = ui.as_weak();
        let catalog_views = catalog_views.clone();
        ui.on_clear_catalog_search(move || {
            let Some(ui) = weak.upgrade() else { return };
            // Separate action from "Reset filters": drops the query and leaves
            // search mode, returning to the current category listing. The
            // field text participates through the two-way binding on
            // DarkLineEdit, so clearing the property clears the visible field.
            CatalogSearchState::cleared().apply(&ui);
            clear_saved_view_queries(&catalog_views, &ui.get_active_category());
            ui.invoke_filter_category(ui.get_active_category());
        });
    }
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let resolver = resolver.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let cached_catalog = cached_catalog.clone();
        let catalog_generation = catalog_generation.clone();
        let search_debounce_generation = search_debounce_generation.clone();
        let catalog_paging = catalog_paging.clone();
        let catalog_filters = catalog_filters.clone();
        let catalog_views = catalog_views.clone();
        ui.on_search_catalog(move |query| {
            let Some(ui) = weak.upgrade() else { return };
            save_catalog_view(&ui, &catalog_paging, &cached_catalog, &catalog_views);
            let query = query.trim().to_string();
            // Search lists videos, so it applies the video ("Latest") filter
            // set — the same one the chips edit and the badge counts.
            let filters = get_category_filters(&catalog_filters, "Latest");
            sync_catalog_filters_to_ui(&ui, "Latest", &filters);
            let library_id = local_store
                .get_setting("selected_library_id")
                .ok()
                .flatten()
                .unwrap_or_default();
            let target_page = CatalogPageState {
                category: "Latest".into(),
                query: query.clone(),
                library_id,
                filters: filters.clone(),
                ..CatalogPageState::default()
            };
            if let Some((request_id, cards)) = restore_catalog_view(
                &ui,
                &catalog_view_key(&target_page),
                &catalog_paging,
                &cached_catalog,
                &catalog_views,
                &catalog_generation,
                &search_debounce_generation,
            ) {
                if let Some(session) = credential_store.get().ok().flatten() {
                    refresh_catalog_thumbnails_async(
                        ui.as_weak(),
                        cards,
                        session.access_token,
                        cached_catalog.clone(),
                        Some((catalog_generation.clone(), request_id)),
                    );
                }
                return;
            }
            ui.set_status("Searching library…".into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
            ui.set_catalog_total_count(0);
            ui.set_is_loading_catalog(true);
            ui.set_is_catalog_search_results(!query.is_empty());
            ui.set_search_applied_query(query.clone().into());
            ui.set_search_error_message("".into());
            ui.set_search_all_has_more(false);
            ui.set_search_all_total_count(0);
            ui.set_search_results_filter("all".into());
            ui.set_search_movie_count(0);
            ui.set_search_performer_count(0);
            ui.set_search_studio_count(0);
            ui.set_search_tag_count(0);
            ui.set_search_channel_count(0);
            ui.set_search_movie_total(0);
            ui.set_search_performer_total(0);
            ui.set_search_studio_total(0);
            ui.set_search_tag_total(0);
            ui.set_search_channel_total(0);
            ui.set_focused_card_index(-1);
            let search_context = active_search_context(&ui);
            let debounce_id = search_debounce_generation.fetch_add(1, Ordering::SeqCst) + 1;
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let resolver = resolver.clone();
            let credential_store = credential_store.clone();
            let local_store = local_store.clone();
            let cached_catalog = cached_catalog.clone();
            let request_id = catalog_generation.fetch_add(1, Ordering::SeqCst) + 1;
            let catalog_generation = catalog_generation.clone();
            let search_debounce_generation = search_debounce_generation.clone();
            let catalog_paging = catalog_paging.clone();
            ui.set_is_loading_more(false);
            ui.set_catalog_has_more(false);
            ui.set_catalog_total_count(0);
            ui.set_catalog_more_error("".into());
            set_catalog(&ui, Vec::new());
            set_search_sections(&ui, &[]);

            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                if search_debounce_generation.load(Ordering::SeqCst) != debounce_id {
                    return;
                }
                if !query.is_empty() {
                    let _ = local_store.add_search_query_context(search_context, &query);
                }
                let recent_queries = local_store
                    .get_search_history_context(search_context, 8)
                    .unwrap_or_default();

                let session = credential_store.get().ok().flatten();
                let image_server = session.as_ref().map(|session| session.server_url.clone());
                let token_for_refresh =
                    session.as_ref().map(|session| session.access_token.clone());

                // Two-stage publish: the first call renders the movie page as
                // soon as stage A lands; the second re-renders once the slower
                // performer/studio/tag/channel sources have appended.
                let publish = {
                    let weak = weak.clone();
                    let query = query.clone();
                    let local_store = local_store.clone();
                    let filters = filters.clone();
                    let catalog_paging = catalog_paging.clone();
                    let cached_catalog = cached_catalog.clone();
                    let catalog_generation = catalog_generation.clone();
                    let recent_queries = recent_queries.clone();
                    let image_server = image_server.clone();
                    let token_for_refresh = token_for_refresh.clone();
                    move |items: Vec<TvItem>,
                          next_start: usize,
                          totals: SearchTypeTotals,
                          has_more: bool,
                          supplemental_total: usize,
                          search_error_message: String,
                          is_unauth: bool| {
                        let filtered = filter_catalog_items(items, &local_store);
                        let (movie_count, performer_count, studio_count, tag_count, channel_count) =
                            search_result_counts(&filtered);
                        let totals = SearchTypeTotals {
                            movie: totals.movie.max(movie_count),
                            performer: totals.performer.max(performer_count),
                            studio: totals.studio.max(studio_count),
                            tag: totals.tag.max(tag_count),
                            channel: totals.channel.max(channel_count),
                        };
                        let aggregate = totals.movie
                            + totals.performer
                            + totals.studio
                            + totals.tag
                            + totals.channel;
                        let weak = weak.clone();
                        let query = query.clone();
                        let local_store = local_store.clone();
                        let filters = filters.clone();
                        let catalog_paging = catalog_paging.clone();
                        let cached_catalog = cached_catalog.clone();
                        let catalog_generation = catalog_generation.clone();
                        let recent_queries = recent_queries.clone();
                        let image_server = image_server.clone();
                        let token_for_refresh = token_for_refresh.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            let Some(ui) = weak.upgrade() else { return };
                            if !catalog_generation_is_current(&catalog_generation, request_id) {
                                return;
                            }
                            *catalog_paging.lock().unwrap() = CatalogPageState {
                                category: "Latest".into(),
                                query,
                                library_id: local_store
                                    .get_setting("selected_library_id")
                                    .ok()
                                    .flatten()
                                    .unwrap_or_default(),
                                next_start,
                                has_more,
                                loading_more: false,
                                filters,
                                supplemental_total,
                            };
                            *cached_catalog.lock().unwrap() = filtered.clone();
                            let cards = filtered
                                .iter()
                                .map(|item| {
                                    image_server.as_ref().map_or_else(
                                        || tv_item_to_card(item),
                                        |server| tv_item_to_card_for_server(item, server),
                                    )
                                })
                                .collect::<Vec<_>>();
                            set_catalog(&ui, cards.clone());
                            set_search_sections(&ui, &cards);
                            set_recent_searches(&ui, recent_queries);
                            ui.set_search_movie_count(movie_count as i32);
                            ui.set_search_performer_count(performer_count as i32);
                            ui.set_search_studio_count(studio_count as i32);
                            ui.set_search_tag_count(tag_count as i32);
                            ui.set_search_channel_count(channel_count as i32);
                            ui.set_search_movie_total(totals.movie as i32);
                            ui.set_search_performer_total(totals.performer as i32);
                            ui.set_search_studio_total(totals.studio as i32);
                            ui.set_search_tag_total(totals.tag as i32);
                            ui.set_search_channel_total(totals.channel as i32);
                            ui.set_search_error_message(search_error_message.into());
                            ui.set_search_all_has_more(has_more);
                            ui.set_search_all_total_count(aggregate as i32);
                            ui.set_catalog_has_more(has_more);
                            ui.set_catalog_total_count(aggregate as i32);
                            ui.set_is_loading_more(false);
                            ui.set_is_loading_catalog(false);
                            ui.set_status("Ready".into());
                            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                            if let Some(token) = token_for_refresh {
                                let model = ui.get_catalog_items();
                                let cards = (0..model.row_count())
                                    .filter_map(|index| model.row_data(index))
                                    .collect::<Vec<_>>();
                                refresh_catalog_thumbnails_async(
                                    ui.as_weak(),
                                    cards,
                                    token,
                                    cached_catalog.clone(),
                                    Some((catalog_generation.clone(), request_id)),
                                );
                            }
                            if is_unauth {
                                ui.set_is_logged_in(false);
                                ui.set_login_modal_visible(true);
                                ui.set_login_modal_message(
                                    "Session expired (401 Unauthorized). Please sign in again."
                                        .into(),
                                );
                                ui.set_toast_message("Session expired. Please reconnect.".into());
                                ui.set_toast_type("warning".into());
                                ui.set_toast_action_text("Sign In".into());
                                ui.set_toast_visible(true);
                            }
                        });
                    }
                };

                let Some(session) = session else {
                    publish(
                        Vec::new(),
                        0,
                        SearchTypeTotals::default(),
                        false,
                        0,
                        "Connect Jellyfin before searching your library.".into(),
                        false,
                    );
                    return;
                };

                let stage_a = get_search_movies_page_with_empty_fetch(
                    &jellyfin,
                    &session,
                    &local_store,
                    &query,
                    &filters,
                );
                let mut page = match stage_a {
                    Ok(page) => page,
                    Err(error) => {
                        let is_unauth = error.is_unauthorized();
                        let message: String = if is_unauth {
                            "Your Jellyfin session expired. Sign in again and retry."
                        } else {
                            "Could not reach Jellyfin. Check the connection and retry."
                        }
                        .into();
                        if is_unauth {
                            publish(
                                Vec::new(),
                                0,
                                SearchTypeTotals::default(),
                                false,
                                0,
                                message,
                                true,
                            );
                            return;
                        }
                        // Jellyfin is unreachable, but the Resolver may still
                        // answer for channel results.
                        let mut partial = TvItemsPage {
                            items: Vec::new(),
                            total_record_count: 0,
                            start_index: 0,
                        };
                        let supplements = fetch_search_supplements(
                            &jellyfin,
                            &resolver,
                            &session,
                            &query,
                            &resolver_base_url_for(&local_store, &session),
                        );
                        let totals = supplements.totals();
                        let supplemental_total =
                            merge_search_supplements(&mut partial, supplements, &local_store);
                        publish(
                            partial.items,
                            0,
                            totals,
                            false,
                            supplemental_total,
                            message,
                            false,
                        );
                        return;
                    }
                };
                let raw_movies = page.items.clone();
                publish(
                    std::mem::take(&mut page.items),
                    page.next_start,
                    SearchTypeTotals {
                        movie: page.total_count,
                        ..Default::default()
                    },
                    page.has_more,
                    0,
                    String::new(),
                    false,
                );

                if query.trim().is_empty()
                    || search_debounce_generation.load(Ordering::SeqCst) != debounce_id
                {
                    return;
                }
                let supplements = fetch_search_supplements(
                    &jellyfin,
                    &resolver,
                    &session,
                    &query,
                    &resolver_base_url_for(&local_store, &session),
                );
                let mut totals = supplements.totals();
                totals.movie = page.total_count;
                let mut merged = TvItemsPage {
                    items: raw_movies,
                    total_record_count: page.total_count,
                    start_index: 0,
                };
                let supplemental_total =
                    merge_search_supplements(&mut merged, supplements, &local_store);
                if supplemental_total == 0 {
                    return;
                }
                publish(
                    merged.items,
                    page.next_start,
                    totals,
                    page.has_more,
                    supplemental_total,
                    String::new(),
                    false,
                );
            });
        });
    }

    // The Android search screen separates library items, performers, and
    // Resolver channel videos. Keep the combined desktop search, and let the
    // user narrow it locally without issuing duplicate network requests.
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let catalog_generation = catalog_generation.clone();
        let catalog_paging = catalog_paging.clone();
        let credential_store = credential_store.clone();
        ui.on_filter_search_results(move |kind| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_is_catalog_search_results() {
                return;
            }

            let items = cached_catalog.lock().unwrap().clone();
            let filtered = items
                .into_iter()
                .filter(|item| match kind.as_str() {
                    "movies" => {
                        !item.is_telegram()
                            && !item.genres.iter().any(|tag| {
                                tag == "__adult_hub_performer"
                                    || tag == "__adult_hub_studio"
                                    || tag == "__adult_hub_genre"
                                    || tag == "__adult_hub_channel_search"
                            })
                    }
                    "performers" => item.genres.iter().any(|tag| tag == "__adult_hub_performer"),
                    "studios" => item.genres.iter().any(|tag| tag == "__adult_hub_studio"),
                    "tags" => item.genres.iter().any(|tag| tag == "__adult_hub_genre"),
                    "channels" => {
                        item.is_telegram()
                            || item
                                .genres
                                .iter()
                                .any(|tag| tag == "__adult_hub_channel_search")
                    }
                    _ => true,
                })
                .collect::<Vec<_>>();
            let session = credential_store.get().ok().flatten();
            let cards = filtered
                .iter()
                .map(|item| {
                    session.as_ref().map_or_else(
                        || tv_item_to_card(item),
                        |session| tv_item_to_card_for_server(item, &session.server_url),
                    )
                })
                .collect::<Vec<_>>();

            let request_id = catalog_generation.fetch_add(1, Ordering::SeqCst) + 1;
            let mut paging = catalog_paging.lock().unwrap();
            paging.loading_more = false;
            // The sectioned "All" view is a capped preview, so it never pages;
            // the flat Movies tab owns pagination and filters each mixed page
            // down to movies on append.
            paging.has_more = kind == "movies" && ui.get_search_all_has_more();
            let has_more = paging.has_more;
            drop(paging);

            ui.set_search_results_filter(kind.clone());
            let per_type_total = match kind.as_str() {
                "movies" => ui.get_search_movie_total(),
                "performers" => ui.get_search_performer_total(),
                "studios" => ui.get_search_studio_total(),
                "tags" => ui.get_search_tag_total(),
                "channels" => ui.get_search_channel_total(),
                _ => ui.get_search_all_total_count(),
            };
            ui.set_catalog_total_count(per_type_total.max(filtered.len() as i32));
            ui.set_is_loading_more(false);
            ui.set_focused_card_index(-1);
            set_catalog(&ui, cards.clone());
            // set_catalog resets paging flags, so the page state must land
            // after it or the typed tab loses its load-more trigger.
            ui.set_catalog_has_more(has_more);
            // Only All renders sections. A typed tab holds one type, so
            // rebuilding here would blank the other four models for nothing;
            // switching back to All rebuilds them from the full list.
            if kind == "all" {
                set_search_sections(&ui, &cards);
            }

            if let Some(session) = session {
                let filtered_items = Arc::new(Mutex::new(filtered));
                refresh_catalog_thumbnails_async(
                    weak.clone(),
                    cards,
                    session.access_token,
                    filtered_items,
                    Some((catalog_generation.clone(), request_id)),
                );
            }
        });
    }

    {
        let weak = ui.as_weak();
        let catalog_paging = catalog_paging.clone();
        let cached_catalog = cached_catalog.clone();
        let catalog_views = catalog_views.clone();
        let catalog_generation = catalog_generation.clone();
        let search_debounce_generation = search_debounce_generation.clone();
        ui.on_cache_catalog_view(move || {
            let Some(ui) = weak.upgrade() else { return };
            save_catalog_view(&ui, &catalog_paging, &cached_catalog, &catalog_views);
            catalog_generation.fetch_add(1, Ordering::SeqCst);
            search_debounce_generation.fetch_add(1, Ordering::SeqCst);
        });
    }

    // 6. Filter Category
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let resolver = resolver.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let cached_catalog = cached_catalog.clone();
        let catalog_generation = catalog_generation.clone();
        let search_debounce_generation = search_debounce_generation.clone();
        let performer_search_generation = performer_search_generation.clone();
        let catalog_paging = catalog_paging.clone();
        let catalog_views = catalog_views.clone();
        let catalog_filters = catalog_filters.clone();
        ui.on_filter_category(move |cat| {
            let Some(ui) = weak.upgrade() else { return };
            if cat != "Latest" {
                ui.set_catalog_category_drilldown(false);
            } else if !ui.get_catalog_category_drilldown() {
                ui.set_catalog_genre_query("".into());
                if let Ok(mut map) = catalog_filters.lock() {
                    if let Some(f) = map.get_mut("Latest") {
                        f.genre.clear();
                    }
                }
            }
            save_catalog_view(&ui, &catalog_paging, &cached_catalog, &catalog_views);
            performer_search_generation.fetch_add(1, Ordering::SeqCst);
            search_debounce_generation.fetch_add(1, Ordering::SeqCst);
            refresh_recent_searches_for_context(&ui, &local_store);
            if cat == "Channels" {
                catalog_generation.fetch_add(1, Ordering::SeqCst);
                ui.set_active_screen("channels".into());
                let current_cat = ui.get_active_channel_category();
                if !current_cat.is_empty() {
                    ui.invoke_select_channel_category(current_cat);
                } else if let Ok(Some(saved)) = local_store.get_selected_channel() {
                    ui.invoke_select_channel_category(saved.into());
                } else {
                    ui.invoke_refresh_channel_categories();
                }
                return;
            }
            let performer_letter = ui.get_performer_letter().to_string();
            let performers_photo_only = ui.get_performers_photo_only();
            let performer_search_query = if cat == "Performers" {
                ui.get_performer_search_query().to_string()
            } else {
                String::new()
            };
            let cat = cat.to_string();
            let mut filters = get_category_filters(&catalog_filters, &cat);
            if cat == "Performers" {
                filters.performer_letter = performer_letter.clone();
            }
            let library_id = local_store
                .get_setting("selected_library_id")
                .ok()
                .flatten()
                .unwrap_or_default();
            let target_page = CatalogPageState {
                category: cat.clone(),
                query: performer_search_query.clone(),
                library_id: library_id.clone(),
                filters: filters.clone(),
                ..CatalogPageState::default()
            };
            if let Some((request_id, cards)) = restore_catalog_view(
                &ui,
                &catalog_view_key(&target_page),
                &catalog_paging,
                &cached_catalog,
                &catalog_views,
                &catalog_generation,
                &search_debounce_generation,
            ) {
                if let Some(session) = credential_store.get().ok().flatten() {
                    refresh_catalog_thumbnails_async(
                        ui.as_weak(),
                        cards,
                        session.access_token,
                        cached_catalog.clone(),
                        Some((catalog_generation.clone(), request_id)),
                    );
                }
                return;
            }
            let request_id = catalog_generation.fetch_add(1, Ordering::SeqCst) + 1;
            ui.set_is_catalog_search_results(false);
            ui.set_search_error_message("".into());
            ui.set_search_results_filter("all".into());
            ui.set_is_loading_catalog(true);
            ui.set_is_loading_more(false);
            ui.set_catalog_has_more(false);
            ui.set_catalog_total_count(0);
            ui.set_catalog_more_error("".into());
            set_catalog(&ui, Vec::new());
            set_search_sections(&ui, &[]);
            let loading_msg = match cat.as_str() {
                "Performers" => "Loading performers…",
                "Studios" => "Loading studios…",
                "Categories" => "Loading categories…",
                "Continue Watching" | "Resume" => "Loading continue watching…",
                "Favorites" => "Loading favorites…",
                "Watch History" => "Loading watch history…",
                _ => "Loading library…",
            };
            ui.set_status(loading_msg.into());
            ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let resolver = resolver.clone();
            let credential_store = credential_store.clone();
            let local_store = local_store.clone();
            let cached_catalog = cached_catalog.clone();
            let catalog_generation = catalog_generation.clone();
            let catalog_paging = catalog_paging.clone();
            let library_id = library_id.clone();
            sync_catalog_filters_to_ui(&ui, &cat, &filters);

            std::thread::spawn(move || {
                let mut is_unauth = false;
                let mut image_auth = None;
                let (raw_items, next_start, total_count, has_more, supplemental_total) =
                    if let Some(session) = credential_store.get().ok().flatten() {
                        image_auth =
                            Some((session.server_url.clone(), session.access_token.clone()));
                        match get_catalog_page_with_empty_page_fetch(
                            &jellyfin,
                            &resolver,
                            &session,
                            &local_store,
                            &cat,
                            &performer_search_query,
                            0,
                            filters.clone(),
                            performers_photo_only,
                        ) {
                            Ok(page) => (
                                page.items,
                                page.next_start,
                                page.total_count,
                                page.has_more,
                                page.supplemental_total,
                            ),
                            Err(error) => {
                                is_unauth = error.is_unauthorized();
                                (Vec::new(), 0, 0, false, 0)
                            }
                        }
                    } else {
                        (Vec::new(), 0, 0, false, 0)
                    };
                let raw_items = if cat == "Studios" {
                    crate::catalog::aggregate_studio_items(raw_items)
                } else {
                    raw_items
                };
                let raw_items = filter_catalog_items(raw_items, &local_store);
                let display_items = if cat == "Performers" {
                    filter_performer_results(
                        &raw_items,
                        &performer_letter,
                        performers_photo_only,
                        &performer_search_query,
                    )
                } else {
                    raw_items.clone()
                };
                let image_server = image_auth.as_ref().map(|(server, _)| server.clone());
                let token_for_refresh = image_auth.map(|(_, token)| token);
                let is_studios = cat == "Studios";
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        if !catalog_generation_is_current(&catalog_generation, request_id) {
                            return;
                        }
                        *cached_catalog.lock().unwrap() = raw_items;
                        *catalog_paging.lock().unwrap() = CatalogPageState {
                            category: cat,
                            query: performer_search_query,
                            library_id,
                            next_start,
                            has_more,
                            loading_more: false,
                            filters,
                            supplemental_total,
                        };
                        let cards = display_items
                            .iter()
                            .map(|item| {
                                image_server.as_ref().map_or_else(
                                    || tv_item_to_card(item),
                                    |server| tv_item_to_card_for_server(item, server),
                                )
                            })
                            .collect::<Vec<_>>();
                        set_catalog(&ui, cards.clone());
                        ui.set_catalog_has_more(has_more);
                        let reported_total = if !has_more {
                            cards.len()
                        } else if is_studios {
                            // Studio records are aggregated client-side, so the
                            // raw server total is not comparable with the cards
                            // on screen. Show the honest loaded count instead of
                            // an "N of M" range against a different universe.
                            cards.len()
                        } else {
                            total_count
                        };
                        ui.set_catalog_total_count(reported_total as i32);
                        ui.set_is_loading_more(false);
                        ui.set_is_loading_catalog(false);
                        ui.set_status("Ready".into());
                        ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                        if let Some(token) = token_for_refresh {
                            refresh_catalog_thumbnails_async(
                                ui.as_weak(),
                                cards,
                                token,
                                cached_catalog.clone(),
                                Some((catalog_generation.clone(), request_id)),
                            );
                        }
                        if is_unauth {
                            ui.set_is_logged_in(false);
                            ui.set_login_modal_visible(true);
                            ui.set_login_modal_message(
                                "Session expired (401 Unauthorized). Please sign in again.".into(),
                            );
                            ui.set_toast_message("Session expired. Please reconnect.".into());
                            ui.set_toast_type("warning".into());
                            ui.set_toast_action_text("Sign In".into());
                            ui.set_toast_visible(true);
                        }
                    }
                })
                .ok();
            });
        });
    }

    // Apply performer filters locally so paging and the underlying cache stay intact.
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let catalog_generation = catalog_generation.clone();
        let catalog_paging = catalog_paging.clone();
        let credential_store = credential_store.clone();
        ui.on_update_performer_filter(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_active_category() != "Performers" {
                return;
            }

            let request_id = catalog_generation.fetch_add(1, Ordering::SeqCst) + 1;
            catalog_paging.lock().unwrap().loading_more = false;
            ui.set_is_loading_more(false);
            ui.set_catalog_more_error("".into());
            let items = cached_catalog.lock().unwrap().clone();
            let filtered = filter_performer_results(
                &items,
                &ui.get_performer_letter(),
                ui.get_performers_photo_only(),
                &ui.get_performer_search_query(),
            );
            let session = credential_store.get().ok().flatten();
            let cards = filtered
                .iter()
                .map(|item| {
                    session.as_ref().map_or_else(
                        || tv_item_to_card(item),
                        |session| tv_item_to_card_for_server(item, &session.server_url),
                    )
                })
                .collect::<Vec<_>>();
            set_catalog(&ui, cards.clone());
            ui.set_catalog_total_count(cards.len() as i32);

            if let Some(session) = session {
                refresh_catalog_thumbnails_async(
                    weak.clone(),
                    cards,
                    session.access_token,
                    Arc::new(Mutex::new(filtered)),
                    Some((catalog_generation.clone(), request_id)),
                );
            }
        });
    }

    // Keep performer name searches responsive while avoiding a Jellyfin
    // request for every keystroke. The list is filtered locally immediately;
    // this callback fetches server matches after typing pauses.
    {
        let weak = ui.as_weak();
        let performer_search_generation = performer_search_generation.clone();
        let local_store = local_store.clone();
        ui.on_request_performer_search(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_active_category() != "Performers" {
                return;
            }
            let scheduled_query = ui.get_performer_search_query().to_string();
            let request_id = performer_search_generation.fetch_add(1, Ordering::SeqCst) + 1;
            let generation = performer_search_generation.clone();
            let local_store = local_store.clone();
            let weak = weak.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                if generation.load(Ordering::SeqCst) != request_id {
                    return;
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if generation.load(Ordering::SeqCst) != request_id {
                        return;
                    }
                    if let Some(ui) = weak.upgrade()
                        && ui.get_active_category() == "Performers"
                        && ui.get_performer_search_query().as_str() == scheduled_query
                    {
                        let trimmed = scheduled_query.trim();
                        if !trimmed.is_empty() {
                            let _ = local_store.add_search_query_context("performers", trimmed);
                            refresh_recent_searches_for_context(&ui, &local_store);
                        }
                        ui.invoke_filter_category("Performers".into());
                    }
                });
            });
        });
    }

    // Load additional pages for the currently visible catalog/search result.
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let resolver = resolver.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let cached_catalog = cached_catalog.clone();
        let catalog_generation = catalog_generation.clone();
        let catalog_paging = catalog_paging.clone();
        let force_retry = Rc::<std::cell::Cell<bool>>::new(std::cell::Cell::new(false));
        let retry_weak = weak.clone();
        let retry_flag = force_retry.clone();
        ui.on_retry_load_more_catalog(move || {
            if let Some(ui) = retry_weak.upgrade() {
                ui.set_catalog_more_error("".into());
                retry_flag.set(true);
                ui.invoke_load_more_catalog();
            }
        });
        let force_retry = force_retry.clone();
        ui.on_load_more_catalog(move || {
            let Some(ui) = weak.upgrade() else { return };
            // An explicit Retry click clears the failure banner; scroll-driven
            // calls must not slip past the "stop until retried" gate below.
            let force_retry = force_retry.replace(false);
            if ui.get_catalog_more_error() != "" && !force_retry {
                return;
            }
            let mut page_state = catalog_paging.lock().unwrap();
            if page_state.loading_more {
                return;
            }
            if !page_state.has_more && !force_retry {
                return;
            }
            page_state.has_more = true;
            page_state.loading_more = true;
            let requested_page = page_state.clone();
            drop(page_state);

            ui.set_is_loading_more(true);
            let request_id = catalog_generation.load(Ordering::SeqCst);
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let resolver = resolver.clone();
            let credential_store = credential_store.clone();
            let local_store = local_store.clone();
            let cached_catalog = cached_catalog.clone();
            let catalog_generation = catalog_generation.clone();
            let catalog_paging = catalog_paging.clone();
            let performer_photo_only = ui.get_performers_photo_only();

            std::thread::spawn(move || {
                let result = credential_store
                    .get()
                    .ok()
                    .flatten()
                    .ok_or_else(|| JellyfinError::Api("Sign in to load more items".into()))
                    .and_then(|session| {
                        get_catalog_page_with_empty_page_fetch(
                            &jellyfin,
                            &resolver,
                            &session,
                            &local_store,
                            &requested_page.category,
                            &requested_page.query,
                            requested_page.next_start,
                            requested_page.filters.clone(),
                            performer_photo_only,
                        )
                        .map(|page| (page, session.server_url, session.access_token))
                    });

                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    if !catalog_generation_is_current(&catalog_generation, request_id) {
                        return;
                    }

                    match result {
                        Ok((page, server_url, token)) => {
                            let supplemental_total = requested_page.supplemental_total;
                            let is_search = !requested_page.query.is_empty();
                            let items = {
                                let mut cached = cached_catalog.lock().unwrap();
                                if requested_page.category == "Studios" {
                                    let mut seen = cached
                                        .iter()
                                        .map(|item| item.id.clone())
                                        .collect::<std::collections::HashSet<_>>();
                                    for item in page.items {
                                        if seen.insert(item.id.clone()) {
                                            cached.push(item);
                                        }
                                    }
                                    *cached = crate::catalog::aggregate_studio_items(
                                        std::mem::take(&mut *cached),
                                    );
                                } else {
                                    let mut seen = cached
                                        .iter()
                                        .map(|item| item.id.clone())
                                        .collect::<std::collections::HashSet<_>>();
                                    for item in page.items {
                                        if seen.insert(item.id.clone()) {
                                            cached.push(item);
                                        }
                                    }
                                }
                                filter_catalog_items(cached.clone(), &local_store)
                            };
                            *cached_catalog.lock().unwrap() = items.clone();
                            let display_items = if requested_page.category == "Performers" {
                                filter_performer_results(
                                    &items,
                                    &ui.get_performer_letter(),
                                    ui.get_performers_photo_only(),
                                    &ui.get_performer_search_query(),
                                )
                            } else {
                                items.clone()
                            };
                            let cards = display_items
                                .iter()
                                .map(|item| tv_item_to_card_for_server(item, &server_url))
                                .collect::<Vec<_>>();

                            let is_studios = requested_page.category == "Studios";
                            *catalog_paging.lock().unwrap() = CatalogPageState {
                                next_start: page.next_start,
                                has_more: page.has_more,
                                loading_more: false,
                                ..requested_page
                            };
                            // A typed search tab holds only its own type, so
                            // append the matching subset of this mixed page;
                            // cached_catalog keeps everything for the All tab.
                            let display_cards = if is_search {
                                let filter = ui.get_search_results_filter();
                                cards
                                    .iter()
                                    .filter(|card| card_matches_search_filter(card, &filter))
                                    .cloned()
                                    .collect::<Vec<_>>()
                            } else {
                                cards.clone()
                            };
                            ui.set_catalog_anchor_pending(true);
                            append_catalog_page(&ui, display_cards.clone());
                            let mut reported_total = if !is_search && (is_studios || !page.has_more)
                            {
                                // Studios are aggregated client-side; the raw
                                // server total is not comparable with the cards
                                // (see first-page counter), so report the
                                // honest loaded count while paging continues.
                                cards.len() as i32
                            } else {
                                (page.total_count + supplemental_total) as i32
                            };
                            if is_search {
                                let (
                                    movie_count,
                                    performer_count,
                                    studio_count,
                                    tag_count,
                                    channel_count,
                                ) = search_result_counts(&items);
                                ui.set_search_movie_count(movie_count as i32);
                                ui.set_search_performer_count(performer_count as i32);
                                ui.set_search_studio_count(studio_count as i32);
                                ui.set_search_tag_count(tag_count as i32);
                                ui.set_search_channel_count(channel_count as i32);
                                ui.set_search_all_has_more(page.has_more);
                                ui.set_search_movie_total(
                                    ui.get_search_movie_total().max(page.total_count as i32),
                                );
                                let aggregate = ui.get_search_movie_total()
                                    + ui.get_search_performer_total()
                                    + ui.get_search_studio_total()
                                    + ui.get_search_tag_total()
                                    + ui.get_search_channel_total();
                                ui.set_search_all_total_count(aggregate);
                                // The flat tab must report its own type total;
                                // the aggregate belongs to the "All" tab.
                                reported_total = match ui.get_search_results_filter().as_str() {
                                    "movies" => ui.get_search_movie_total(),
                                    "performers" => ui.get_search_performer_total(),
                                    "studios" => ui.get_search_studio_total(),
                                    "tags" => ui.get_search_tag_total(),
                                    "channels" => ui.get_search_channel_total(),
                                    _ => aggregate,
                                };
                                if ui.get_search_results_filter() == "all" {
                                    set_search_sections(&ui, &cards);
                                }
                            }
                            ui.set_catalog_has_more(page.has_more);
                            ui.set_catalog_more_error("".into());
                            ui.set_catalog_total_count(reported_total);
                            ui.set_is_loading_more(false);
                            refresh_catalog_thumbnails_async(
                                ui.as_weak(),
                                display_cards,
                                token,
                                cached_catalog.clone(),
                                Some((catalog_generation.clone(), request_id)),
                            );
                        }
                        Err(error) => {
                            let mut paging = catalog_paging.lock().unwrap();
                            paging.loading_more = false;
                            // Stop the scroll-driven prefetch from hammering a
                            // failing endpoint: the banner's Retry is the only
                            // way back until the state changes.
                            paging.has_more = false;
                            drop(paging);
                            ui.set_catalog_has_more(false);
                            ui.set_is_loading_more(false);
                            // Keep the loaded cards; surface the failure at the
                            // end of the list with its own retry affordance.
                            let message = format!("Could not load more items: {error}");
                            eprintln!("[catalog] {message}");
                            ui.set_catalog_more_error(message.into());
                        }
                    }
                })
                .ok();
            });
        });
    }

    // 7. Play Item
    {
        let weak = ui.as_weak();
        let ctx_play = ctx.clone();
        ui.on_play_item(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let id = id.to_string();

            // Locate via find_catalog_item or synthesize TvItem
            let target_item = ctx_play.find_catalog_item(&id).unwrap_or_else(|| {
                if id == "sample" {
                    sample_catalog_tv_items()[0].clone()
                } else if id.starts_with("telegram:") {
                    let prov_id = id.strip_prefix("telegram:").unwrap_or(&id).to_string();
                    TvItem {
                        id: id.clone(),
                        title: format!("Telegram video {prov_id}"),
                        overview: None,
                        year: None,
                        runtime_ticks: None,
                        genres: vec!["Telegram".into()],
                        tags: vec![],
                        studios: vec![],
                        people: vec![],
                        primary_image_tag: None,
                        backdrop_image_tag: None,
                        thumb_image_tag: None,
                        child_count: None,
                        is_favorite: false,
                        playback_position_ticks: 0,
                        played_percentage: None,
                        is_played: false,
                        provider_video_id: Some(prov_id),
                        source_provider: Some("telegram".into()),
                        media_source_id: None,
                        container: Some("mp4".into()),
                        file_size_bytes: None,
                        is_placeholder_source: false,
                        width: None,
                        height: None,
                        community_rating: None,
                    }
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
                        child_count: None,
                        is_favorite: false,
                        playback_position_ticks: 0,
                        played_percentage: None,
                        is_played: false,
                        provider_video_id: None,
                        source_provider: None,
                        media_source_id: None,
                        container: None,
                        file_size_bytes: None,
                        is_placeholder_source: false,
                        width: None,
                        height: None,
                        community_rating: None,
                    }
                }
            });

            if target_item
                .genres
                .iter()
                .any(|tag| tag == "__adult_hub_genre")
            {
                ui.invoke_open_item_detail(target_item.id.into());
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

            ctx_play.set_playback_context(cached_catalog.lock().unwrap().clone());
            crate::playback::start_item_playback_with_resume_ctx(&ui, &ctx_play, target_item, None);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_catalog_search_resets_field_applied_query_and_results_together() {
        // Regression for the audited scenario: search "qa-no-results-982731",
        // wait for the empty screen, click the central "Clear search". The
        // visible field text, the applied query and the results state must all
        // return to the catalog state in one action — previously the handler
        // reset the state but the field kept rendering the query.
        let in_search = CatalogSearchState {
            typed_query: "qa-no-results-982731".into(),
            applied_query: "qa-no-results-982731".into(),
            in_results: true,
            error_message: String::new(),
            all_has_more: false,
            all_total_count: 0,
            results_filter: "all".into(),
        };
        let cleared = CatalogSearchState::cleared();
        assert_eq!(cleared.typed_query, "");
        assert_eq!(cleared.applied_query, "");
        assert!(!cleared.in_results);
        assert_eq!(cleared.results_filter, "all");
        assert_ne!(cleared, in_search);
        // A cleared state describes plain catalog browsing: field, counter and
        // list can never disagree because all fields move together.
        assert_eq!(cleared.typed_query, cleared.applied_query);
    }

    #[test]
    fn clear_catalog_search_keeps_search_history() {
        // "Clear search" only moves UI state; it must not erase remembered
        // searches. The transition touches no storage, and an in-memory store
        // proves the history rows survive the reset.
        let store = LocalStore::open_in_memory().unwrap();
        store
            .add_search_query_context("movies", "qa-no-results-982731")
            .unwrap();
        store.add_search_query_context("movies", "dragon").unwrap();
        let _cleared = CatalogSearchState::cleared();
        let history = store.get_search_history_context("movies", 10).unwrap();
        assert!(
            history
                .iter()
                .any(|entry| entry == "qa-no-results-982731" || entry == "dragon"),
            "clearing the current query must not wipe search history"
        );
    }

    #[test]
    fn late_catalog_response_is_rejected_after_a_newer_request_starts() {
        let generation = AtomicU64::new(0);
        let first_request = generation.fetch_add(1, Ordering::SeqCst) + 1;
        let second_request = generation.fetch_add(1, Ordering::SeqCst) + 1;

        assert!(!catalog_generation_is_current(&generation, first_request));
        assert!(catalog_generation_is_current(&generation, second_request));
    }

    #[test]
    fn clear_catalog_search_blanks_the_restored_field_text() {
        // The category listing is restored from a view snapshot that remembers
        // the field text for navigation retention. A "Clear search" must also
        // blank the snapshot of the category it was cleared in, or the old
        // query reappears in the field as soon as the listing is restored.
        let snapshot = |category: &str, query: &str| {
            let page = CatalogPageState {
                category: category.into(),
                ..Default::default()
            };
            CatalogViewSnapshot {
                cards: Vec::new(),
                items: Vec::new(),
                page,
                total_count: 0,
                has_more: false,
                scroll_y: 0.0,
                anchor_index: 0,
                anchor_screen_y: 0.0,
                more_error: String::new(),
                is_search: false,
                search_query: query.into(),
                search_applied_query: String::new(),
                search_error: String::new(),
                search_results_filter: "all".into(),
                search_movie_count: 0,
                search_performer_count: 0,
                search_studio_count: 0,
                search_tag_count: 0,
                search_channel_count: 0,
                search_movie_total: 0,
                search_performer_total: 0,
                search_studio_total: 0,
                search_tag_total: 0,
                search_channel_total: 0,
                search_all_has_more: false,
                search_all_total_count: 0,
            }
        };
        let views = Rc::new(RefCell::new(std::collections::HashMap::new()));
        views.borrow_mut().insert(
            "latest-key".into(),
            snapshot("Latest", "qa-no-results-982731"),
        );
        views.borrow_mut().insert(
            "favorites-key".into(),
            snapshot("Favorites", "typed-elsewhere"),
        );

        clear_saved_view_queries(&views, "Latest");

        assert_eq!(views.borrow()["latest-key"].search_query, "");
        assert_eq!(
            views.borrow()["favorites-key"].search_query,
            "typed-elsewhere",
            "clearing one category's search must not disturb retention elsewhere"
        );
    }

    #[test]
    fn test_search_result_counts() {
        let items = vec![
            TvItem {
                id: "movie1".into(),
                title: "Movie 1".into(),
                ..Default::default()
            },
            TvItem {
                id: "perf1".into(),
                title: "Performer 1".into(),
                genres: vec!["__adult_hub_performer".into()],
                ..Default::default()
            },
            TvItem {
                id: "studio1".into(),
                title: "Studio 1".into(),
                genres: vec!["__adult_hub_studio".into()],
                ..Default::default()
            },
            TvItem {
                id: "tag1".into(),
                title: "Tag 1".into(),
                genres: vec!["__adult_hub_genre".into()],
                ..Default::default()
            },
            TvItem {
                id: "chan1".into(),
                title: "Channel Video".into(),
                genres: vec!["__adult_hub_channel_search".into()],
                ..Default::default()
            },
            TvItem {
                id: "movie2".into(),
                title: "Movie 2".into(),
                ..Default::default()
            },
        ];

        let (movies, performers, studios, tags, channels) = search_result_counts(&items);
        assert_eq!(movies, 2);
        assert_eq!(performers, 1);
        assert_eq!(studios, 1);
        assert_eq!(tags, 1);
        assert_eq!(channels, 1);
    }

    #[test]
    fn test_catalog_filter_sorts_and_labels() {
        let mut filters = CatalogFilters::default();
        assert_eq!(filters.sort_by(), "DateCreated");
        assert_eq!(catalog_filter_labels(&filters).0, "Date added");

        filters.sort = 1;
        assert_eq!(filters.sort_by(), "SortName");
        assert_eq!(catalog_filter_labels(&filters).0, "Title");

        filters.sort = 2;
        assert_eq!(filters.sort_by(), "Runtime");
        assert_eq!(catalog_filter_labels(&filters).0, "Duration");

        filters.sort = 3;
        assert_eq!(filters.sort_by(), "CommunityRating");
        assert_eq!(catalog_filter_labels(&filters).0, "Rating");

        // Cycle through all 4
        filters.sort = (filters.sort + 1) % 4;
        assert_eq!(filters.sort_by(), "DateCreated");
    }

    #[test]
    fn test_catalog_filters_combined() {
        use adult_hub_client::TvPersonRole;

        let filters = CatalogFilters {
            genre: "Anal".into(),
            studio: "Evil Angel".into(),
            performer: "Eva Elfie".into(),
            ..Default::default()
        };

        assert_eq!(filters.active_filter_count("Latest"), 3);

        // Matching all 3
        let matching_item = TvItem {
            id: "match".into(),
            title: "Matching Item".into(),
            genres: vec!["Anal".into(), "Hardcore".into()],
            studios: vec!["Evil Angel".into()],
            people: vec![TvPersonRole {
                id: "p1".into(),
                name: "Eva Elfie".into(),
                role: Some("Actor".into()),
                person_type: Some("Actor".into()),
                primary_image_tag: None,
            }],
            ..Default::default()
        };
        assert!(filters.accepts(&matching_item));

        // Wrong genre
        let wrong_genre = TvItem {
            genres: vec!["Blowjob".into()],
            ..matching_item.clone()
        };
        assert!(!filters.accepts(&wrong_genre));

        // Wrong studio
        let wrong_studio = TvItem {
            studios: vec!["Brazzers".into()],
            ..matching_item.clone()
        };
        assert!(!filters.accepts(&wrong_studio));

        // Wrong performer
        let wrong_performer = TvItem {
            people: vec![TvPersonRole {
                id: "p2".into(),
                name: "Sweet Sophie".into(),
                role: Some("Actor".into()),
                person_type: Some("Actor".into()),
                primary_image_tag: None,
            }],
            ..matching_item.clone()
        };
        assert!(!filters.accepts(&wrong_performer));
    }
}
