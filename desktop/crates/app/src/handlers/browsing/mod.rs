use super::home_categories::{
    cached_home_categories, catalog_lookup_id, clear_home_categories_cache,
    has_unresolved_raw_title, resume_title_needs_hydration,
};
use crate::catalog::{
    append_catalog_page, filter_catalog_items, refresh_catalog_thumbnails_async,
    sample_catalog_tv_items, set_catalog, tv_item_to_card, tv_item_to_card_for_server,
};
use crate::context::AppContext;
use crate::playback::show_error;
use crate::{HomeCategoryOption, HomeCategoryShelfData, MainWindow, VideoCardData};
use adult_hub_client::{
    AdultHubClient, CatalogTaxonomyQuery, CatalogVideosQuery, ServiceUrlPolicy,
    ServiceUrlValidation, TvItem, TvItemsPage,
};
use adult_hub_storage::{LocalStore, Session};
use slint::{ComponentHandle, Model};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod filters;
mod home;
mod random;
use filters::*;
pub use home::restore_saved_home_data;
use home::{resolve_home_category_ids, set_home_category_options, set_home_shelves};
use random::random_catalog_page;

pub(super) const CATALOG_PAGE_SIZE: usize = 50;
const CHANNEL_SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
#[derive(Debug)]
pub(super) enum CatalogError {
    Unauthorized,
    Api(String),
}

impl CatalogError {
    fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Unauthorized)
    }
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "Session expired"),
            Self::Api(message) => write!(f, "{message}"),
        }
    }
}

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

fn search_context(screen: &str, category: &str, drilldown: bool) -> &'static str {
    if screen == "channels" {
        "channels"
    } else if drilldown {
        "category_drilldown"
    } else {
        match category {
            "Performers" => "performers",
            "Studios" => "studios",
            "Categories" => "tags",
            "Favorites" => "favorites",
            "Watch History" => "history",
            "Continue Watching" | "Resume" => "resume",
            _ => "movies",
        }
    }
}

fn resolved_search_context(screen: &str, category: &str, drilldown: bool, genre: &str) -> String {
    match search_context(screen, category, drilldown) {
        "category_drilldown" => format!("category:{genre}"),
        context => context.to_string(),
    }
}

pub(super) fn search_context_label(
    screen: &str,
    category: &str,
    drilldown: bool,
    genre: &str,
) -> String {
    if screen == "channels" {
        "channels".to_string()
    } else if drilldown {
        format!("category \"{genre}\"")
    } else {
        match category {
            "Performers" => "performers",
            "Studios" => "studios",
            "Categories" => "categories",
            "Favorites" => "favorites",
            "Watch History" => "watch history",
            "Continue Watching" | "Resume" => "continue watching",
            _ => "movies",
        }
        .to_string()
    }
}

pub fn active_search_context(ui: &MainWindow) -> String {
    resolved_search_context(
        ui.get_active_screen().as_str(),
        ui.get_active_category().as_str(),
        ui.get_catalog_category_drilldown(),
        ui.get_catalog_genre_query().as_str(),
    )
}

pub fn refresh_recent_searches_for_context(ui: &MainWindow, store: &LocalStore) {
    let ctx = active_search_context(ui);
    let items = store
        .get_search_history_context(&ctx, 8)
        .unwrap_or_default();
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

#[derive(Clone, Default)]
struct CatalogPageState {
    category: String,
    query: String,
    drilldown: bool,
    /// True only for the global multi-source search results view. Scoped
    /// searches keep a query but continue through the category paging path.
    is_search: bool,
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

#[derive(Clone)]
struct CatalogViewSnapshot {
    cards: Vec<VideoCardData>,
    items: Vec<TvItem>,
    page: CatalogPageState,
    cached_at: std::time::Instant,
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

fn catalog_view_key(page: &CatalogPageState) -> String {
    format!(
        "{}\0{}\0{}\0{}\0{}\0{:?}",
        page.library_id, page.category, page.query, page.drilldown, page.is_search, page.filters
    )
}

/// Drop the remembered field text from every stored view of `category` after
/// an explicit "Clear search" so the old query does not reappear on restore.
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

/// Scoped query pages are cached under their category, query, filters, and
/// drilldown state. Transition callers decide whether the current view should
/// be saved; Clear search opts out so the old results are not filed under an
/// empty-query key.
fn save_catalog_view(
    ui: &MainWindow,
    paging: &Arc<Mutex<CatalogPageState>>,
    cached_items: &Arc<Mutex<Vec<TvItem>>>,
    views: &Rc<RefCell<std::collections::HashMap<String, CatalogViewSnapshot>>>,
) {
    if ui.get_is_loading_catalog() {
        return;
    }
    let mut page = paging
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if page.category.is_empty() {
        return;
    }
    page.loading_more = false;
    let model = ui.get_catalog_items();
    let cards = (0..model.row_count())
        .filter_map(|index| model.row_data(index))
        .collect::<Vec<_>>();
    let snapshot = CatalogViewSnapshot {
        cards,
        items: cached_items
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
        page: page.clone(),
        cached_at: std::time::Instant::now(),
        total_count: ui.get_catalog_total_count(),
        has_more: ui.get_catalog_has_more(),
        scroll_y: ui.get_catalog_scroll_y(),
        anchor_index: ui.get_catalog_anchor_index(),
        anchor_screen_y: ui.get_catalog_anchor_screen_y(),
        more_error: ui.get_catalog_more_error().to_string(),
        is_search: page.is_search,
        search_query: if !page.is_search && page.category != "Performers" && !page.query.is_empty()
        {
            page.query.clone()
        } else {
            ui.get_search_query().to_string()
        },
        search_applied_query: if !page.is_search && !page.query.is_empty() {
            page.query.clone()
        } else {
            ui.get_search_applied_query().to_string()
        },
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
    if !snapshot.is_search && snapshot.cached_at.elapsed() > std::time::Duration::from_secs(60) {
        views.borrow_mut().remove(key);
        return None;
    }
    let request_id = generation.fetch_add(1, Ordering::SeqCst) + 1;
    search_generation.fetch_add(1, Ordering::SeqCst);
    let mut page = snapshot.page.clone();
    page.loading_more = false;
    sync_catalog_filters_to_ui(ui, &page.category, &page.filters);
    ui.set_catalog_category_drilldown(page.drilldown);
    if page.category == "Performers" {
        ui.set_performer_search_query(page.query.clone().into());
    }
    *paging
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = page;
    *cached_items
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot.items.clone();

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
    ui.set_search_channels_loading(false);
    ui.set_search_channel_error("".into());
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

/// Filters that actually drive the visible list. Search results are always
/// video lists, so they use the "Latest" filter set — the same set the badge
/// and the chips show. Outside search, each category owns its filters. Keeping
/// one scope for display and application is what makes query, filters, the
/// active-filter counter and the rendered results describe one state.
/// Continue Watching duplicated the local Watch History list, so saved or
/// restored navigation entries for it land on History instead of a dead view.
fn legacy_catalog_category(category: &str) -> &str {
    match category {
        "Continue Watching" | "Resume" => "Watch History",
        other => other,
    }
}

fn catalog_filter_scope(ui: &MainWindow) -> String {
    if ui.get_is_catalog_search_results() || ui.get_catalog_category_drilldown() {
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

pub(super) struct CatalogPage {
    pub(super) items: Vec<TvItem>,
    pub(super) next_start: usize,
    pub(super) total_count: usize,
    pub(super) has_more: bool,
    pub(super) supplemental_total: usize,
}

fn reported_catalog_total(
    is_search: bool,
    is_studios: bool,
    query: &str,
    has_more: bool,
    page_total: usize,
    cards_len: usize,
) -> i32 {
    let is_studio_search = is_studios && !query.trim().is_empty();
    if !is_search && is_studio_search {
        // Studio search totals count raw server rows before client-side
        // aggregation, and stay stable through the final page.
        page_total as i32
    } else if !is_search && (is_studios || !has_more) {
        cards_len as i32
    } else {
        page_total as i32
    }
}

/// Matches a catalog item against a lowercased, trimmed search query.
///
/// Titles, ids and overviews are matched by substring. A `telegram:` prefix is
/// ignored for id matching so a pasted Telegram reference still finds its item.
/// Older releases keyed some Telegram items by an opaque hash
/// (`telegram:<hash>`) instead of the canonical provider reference. Those rows
/// are still candidates for a title-based remap, but are never valid API ids.
fn is_legacy_telegram_hash_id(id: &str) -> bool {
    id.strip_prefix("telegram:")
        .is_some_and(|rest| !rest.contains(':'))
}

/// Playback id for one entry of the local resume ("Continue watching") shelf.
///
/// Telegram rows are keyed by the provider reference, but older builds stored the
/// catalog id (`telegram:<ref>`) inside the row, and rows written from a catalog
/// item nested the prefix again. Prefixing such a value unconditionally produced
/// `telegram:telegram:…` - an id the catalog and the resolver both reject - so the
/// card opened a 404. Hub rows keep their own catalog id instead of a fake prefix.
fn resume_item_id(video_id: &str) -> Option<String> {
    let raw = video_id.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(full_id) = crate::phash::canonical_telegram_id(raw) {
        return Some(full_id);
    }
    if raw.starts_with("telegram:") {
        // Legacy opaque hash rows are remapped by title further down the shelf.
        let bare = adult_hub_storage::normalize_telegram_provider_id(raw);
        return Some(format!("telegram:{bare}"));
    }
    Some(raw.to_string())
}

fn item_matches_query(item: &TvItem, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let stripped = query.strip_prefix("telegram:").unwrap_or(query).trim();
    item.title.to_lowercase().contains(query)
        || item.id.to_lowercase().contains(query)
        || item
            .provider_video_id
            .as_deref()
            .is_some_and(|provider_id| {
                let provider_id = provider_id.to_lowercase();
                provider_id.contains(query)
                    || (!stripped.is_empty() && provider_id.contains(stripped))
            })
        || item
            .overview
            .as_deref()
            .is_some_and(|overview| overview.to_lowercase().contains(query))
}

/// Replace raw Telegram references with real catalog metadata.
///
/// Resume and history rows can predate canonical ids: the title is the bare
/// provider reference (`channel:message:file`). Look the material up by its
/// canonical id (adding the `telegram:` prefix when needed) and copy only the
/// metadata fields, so local progress keys stay untouched.
fn hydrate_missing_titles(client: &AdultHubClient, items: &mut [TvItem], limit: usize) {
    if items.is_empty() || limit == 0 {
        return;
    }
    let missing_indices = items
        .iter()
        .enumerate()
        .filter(|(_, item)| resume_title_needs_hydration(item))
        .map(|(index, _)| index)
        .take(limit)
        .collect::<Vec<_>>();
    if missing_indices.is_empty() {
        return;
    }
    let hydrated = std::thread::scope(|scope| {
        let handles = missing_indices
            .iter()
            .filter_map(|index| {
                let id = items.get(*index)?.id.clone();
                let lookup = catalog_lookup_id(&id);
                Some(scope.spawn(move || {
                    client
                        .get_catalog_video_detail(&lookup)
                        .ok()
                        .map(TvItem::from)
                        .map(|meta| (*index, meta))
                }))
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok().flatten())
            .collect::<Vec<_>>()
    });
    for (index, meta) in hydrated {
        let Some(entry) = items.get_mut(index) else {
            continue;
        };
        entry.title = meta.title;
        entry.primary_image_tag = meta.primary_image_tag;
        entry.runtime_ticks = entry.runtime_ticks.or(meta.runtime_ticks);
        entry.source_provider = meta.source_provider;
    }
}

fn get_adult_hub_catalog_page(
    session: &Session,
    local_store: &LocalStore,
    category: &str,
    query: &str,
    start_index: usize,
    filters: CatalogFilters,
) -> std::result::Result<CatalogPage, CatalogError> {
    let client = adult_hub_client::AdultHubClient::new(&session.server_url)
        .map_err(|e| CatalogError::Api(format!("Failed to initialize Adult Hub client: {e}")))?
        .with_token(&session.access_token);

    if category == "Watch History" {
        let query = query.trim().to_lowercase();
        let mut history = local_store
            .get_watch_history(100)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|json| serde_json::from_str::<TvItem>(&json).ok())
            .filter(|item| item_matches_query(item, &query) && filters.accepts(item))
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
        let mut items = history
            .into_iter()
            .skip(start_index)
            .take(CATALOG_PAGE_SIZE)
            .collect::<Vec<_>>();
        hydrate_missing_titles(&client, &mut items, 20);
        items.retain(|item| !has_unresolved_raw_title(item));
        let next_start = start_index + items.len();
        return Ok(CatalogPage {
            items,
            next_start,
            total_count,
            has_more: next_start < total_count,
            supplemental_total: 0,
        });
    }

    if category == "Categories" {
        let q_term = (!query.trim().is_empty()).then(|| query.trim());
        let cat_resp = client
            .get_categories(&adult_hub_client::CatalogTaxonomyQuery {
                offset: Some(start_index as u64),
                limit: Some(CATALOG_PAGE_SIZE as u64),
                q: q_term,
            })
            .map_err(|err| {
                if err.is_unauthorized() {
                    CatalogError::Unauthorized
                } else {
                    CatalogError::Api(err.to_string())
                }
            })?;

        let total_count = cat_resp.total_count as usize;
        let has_more = cat_resp.has_more();
        let items: Vec<TvItem> = cat_resp
            .items
            .into_iter()
            .map(|item| item.to_category_tv_item())
            .collect();
        let next_start = start_index + items.len();

        return Ok(CatalogPage {
            items,
            next_start,
            total_count,
            has_more,
            supplemental_total: 0,
        });
    }

    if category == "Studios" {
        let q_term = (!query.trim().is_empty()).then(|| query.trim());
        let studio_resp = client
            .get_studios(&adult_hub_client::CatalogTaxonomyQuery {
                offset: Some(start_index as u64),
                limit: Some(CATALOG_PAGE_SIZE as u64),
                q: q_term,
            })
            .map_err(|err| {
                if err.is_unauthorized() {
                    CatalogError::Unauthorized
                } else {
                    CatalogError::Api(err.to_string())
                }
            })?;

        let total_count = studio_resp.total_count as usize;
        let has_more = studio_resp.has_more();
        let items: Vec<TvItem> = studio_resp
            .items
            .into_iter()
            .map(|item| item.to_studio_tv_item())
            .collect();
        let next_start = start_index + items.len();

        return Ok(CatalogPage {
            items,
            next_start,
            total_count,
            has_more,
            supplemental_total: 0,
        });
    }

    if category == "Performers" {
        let q_term = (!query.trim().is_empty()).then(|| query.trim());
        let letter_term =
            (!filters.performer_letter.trim().is_empty()).then(|| filters.performer_letter.trim());
        let (performer_sort, performer_direction) = filters.performer_sort.query();
        let perf_resp = client
            .get_performers(&adult_hub_client::CatalogPerformersQuery {
                offset: Some(start_index as u64),
                limit: Some(CATALOG_PAGE_SIZE as u64),
                q: q_term,
                letter: letter_term,
                photo_only: filters.performer_photo_only.then_some(true),
                sort: Some(performer_sort),
                direction: Some(performer_direction),
            })
            .map_err(|err| {
                if err.is_unauthorized() {
                    CatalogError::Unauthorized
                } else {
                    CatalogError::Api(err.to_string())
                }
            })?;

        let total_count = perf_resp.total_count as usize;
        let has_more = perf_resp.has_more();
        let items: Vec<TvItem> = perf_resp.items.into_iter().map(TvItem::from).collect();
        let next_start = start_index + items.len();

        return Ok(CatalogPage {
            items,
            next_start,
            total_count,
            has_more,
            supplemental_total: 0,
        });
    }

    if category == "Random" {
        return random_catalog_page(&client, &filters, start_index);
    }

    let sort_str = match filters.sort {
        1 => "title",
        2 => "duration",
        _ => "latest",
    };
    let direction_str = if filters.descending { "desc" } else { "asc" };

    let q_term = (!query.trim().is_empty()).then(|| query.trim());
    let category_filter = (!filters.genre.is_empty()).then_some(filters.genre.as_str());
    let studio_filter = (!filters.studio.is_empty()).then_some(filters.studio.as_str());
    let performer_filter = (!filters.performer.is_empty()).then_some(filters.performer.as_str());

    let is_favorites_category = category == "Favorites";
    let is_fav_filter = is_favorites_category || filters.favorites_only;

    if is_favorites_category
        && q_term.is_none()
        && category_filter.is_none()
        && studio_filter.is_none()
        && performer_filter.is_none()
        && filters.sort == 0
    {
        let fav_resp = client
            .get_favorites(
                Some(start_index as u64),
                Some(CATALOG_PAGE_SIZE as u64),
                Some(sort_str),
                Some(direction_str),
            )
            .map_err(|err| {
                if err.is_unauthorized() {
                    CatalogError::Unauthorized
                } else {
                    CatalogError::Api(err.to_string())
                }
            })?;

        let total_count = fav_resp.total_count as usize;
        let has_more = fav_resp.has_more();
        let items: Vec<TvItem> = fav_resp.items.into_iter().map(TvItem::from).collect();
        let next_start = start_index + items.len();

        return Ok(CatalogPage {
            items,
            next_start,
            total_count,
            has_more,
            supplemental_total: 0,
        });
    }

    let req = adult_hub_client::CatalogVideosQuery {
        offset: Some(start_index as u64),
        limit: Some(CATALOG_PAGE_SIZE as u64),
        sort: Some(sort_str),
        direction: Some(direction_str),
        channel_id: None,
        q: q_term,
        category: category_filter,
        tag: None,
        studio: studio_filter,
        performer: performer_filter,
        performer_id: None,
        min_duration: None,
        is_favorite: is_fav_filter.then_some(true),
        quality: filters.quality.query(),
    };

    let resp = client.get_videos(&req).map_err(|err| {
        if err.is_unauthorized() {
            CatalogError::Unauthorized
        } else {
            CatalogError::Api(err.to_string())
        }
    })?;

    let items: Vec<TvItem> = resp.items.into_iter().map(TvItem::from).collect();
    let total_count = resp.total_count as usize;
    let next_start = start_index + items.len();
    let has_more = resp.has_more;

    Ok(CatalogPage {
        items,
        next_start,
        total_count,
        has_more,
        supplemental_total: 0,
    })
}

/// Random tab pages sample the catalog instead of walking it in order.
///
/// Each call asks the server for a page at a random offset, applies the same
/// local filters as the other video tabs, then shuffles the page. `next_start`
/// is therefore a page counter, not a server offset.
fn get_catalog_page(
    session: &Session,
    local_store: &LocalStore,
    category: &str,
    query: &str,
    start_index: usize,
    filters: CatalogFilters,
) -> std::result::Result<CatalogPage, CatalogError> {
    get_adult_hub_catalog_page(session, local_store, category, query, start_index, filters)
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
                .map(|page| page.items.len())
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

fn fetch_search_supplements(client: &AdultHubClient, query: &str) -> SearchSupplements {
    let taxonomy_query = CatalogTaxonomyQuery {
        offset: Some(0),
        limit: Some(30),
        q: Some(query),
    };
    std::thread::scope(|scope| {
        let people_task = scope.spawn(|| {
            client
                .get_performers(&adult_hub_client::CatalogPerformersQuery {
                    offset: Some(0),
                    limit: Some(30),
                    q: Some(query),
                    letter: None,
                    photo_only: None,
                    sort: Some("name"),
                    direction: None,
                })
                .map(|response| TvItemsPage {
                    items: response.items.into_iter().map(TvItem::from).collect(),
                    total_record_count: response.total_count as usize,
                    start_index: 0,
                })
                .map_err(|_| {
                    crate::diagnostics::GLOBAL_LOG.w(
                        "Search",
                        "Adult Hub performer search failed; preserving results from other sources",
                    );
                })
                .ok()
        });
        let studio_task = scope.spawn(|| {
            client
                .get_studios(&taxonomy_query)
                .map(|response| TvItemsPage {
                    items: response
                        .items
                        .into_iter()
                        .map(|item| item.to_studio_tv_item())
                        .collect(),
                    total_record_count: response.total_count as usize,
                    start_index: 0,
                })
                .map_err(|_| {
                    crate::diagnostics::GLOBAL_LOG.w(
                        "Search",
                        "Adult Hub studio search failed; preserving results from other sources",
                    );
                })
                .ok()
        });
        let tag_task = scope.spawn(|| {
            client
                .get_categories(&taxonomy_query)
                .map(|response| TvItemsPage {
                    items: response
                        .items
                        .into_iter()
                        .map(|item| item.to_category_tv_item())
                        .collect(),
                    total_record_count: response.total_count as usize,
                    start_index: 0,
                })
                .map_err(|_| {
                    crate::diagnostics::GLOBAL_LOG.w(
                        "Search",
                        "Adult Hub category search failed; preserving results from other sources",
                    );
                })
                .ok()
        });
        SearchSupplements {
            people: people_task.join().ok().flatten(),
            studios: studio_task.join().ok().flatten(),
            tags: tag_task.join().ok().flatten(),
            channels: None,
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
        studio_page.items = crate::catalog::aggregate_studio_items(studio_page.items);
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

/// Stage A of catalog search: the videos-only page without the supplemental
/// fan-out.
fn get_search_movies_page_at(
    client: &AdultHubClient,
    query: &str,
    filters: &CatalogFilters,
    start_index: usize,
) -> std::result::Result<CatalogPage, CatalogError> {
    let sort = match filters.sort {
        1 => "title",
        2 => "duration",
        _ => "latest",
    };
    let direction = if filters.descending { "desc" } else { "asc" };
    let response = client
        .get_videos(&CatalogVideosQuery {
            offset: Some(start_index as u64),
            limit: Some(CATALOG_PAGE_SIZE as u64),
            sort: Some(sort),
            direction: Some(direction),
            q: Some(query),
            category: (!filters.genre.is_empty()).then_some(filters.genre.as_str()),
            studio: (!filters.studio.is_empty()).then_some(filters.studio.as_str()),
            performer: (!filters.performer.is_empty()).then_some(filters.performer.as_str()),
            is_favorite: filters.favorites_only.then_some(true),
            quality: filters.quality.query(),
            ..Default::default()
        })
        .map_err(|err| {
            if err.is_unauthorized() {
                CatalogError::Unauthorized
            } else {
                CatalogError::Api(err.to_string())
            }
        })?;
    let total_count = response.total_count as usize;
    let server_count = response.items.len();
    let items = response
        .items
        .into_iter()
        .map(TvItem::from)
        .filter(|item| filters.accepts(item))
        .collect::<Vec<_>>();
    let next_start = start_index + server_count;
    Ok(CatalogPage {
        items,
        next_start,
        total_count,
        has_more: response.has_more,
        supplemental_total: 0,
    })
}

fn get_catalog_page_with_empty_page_fetch(
    session: &Session,
    local_store: &LocalStore,
    category: &str,
    query: &str,
    start_index: usize,
    filters: CatalogFilters,
    performer_photo_only: bool,
) -> std::result::Result<CatalogPage, CatalogError> {
    let mut page = get_catalog_page(
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
                } else if ui.get_active_category() == "Performers" {
                    String::new()
                } else {
                    ui.get_search_applied_query().to_string()
                },
                search_results_filter: ui.get_search_results_filter().to_string(),
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
        "performer-details" => {
            let pid = ui.get_performer_id().to_string();
            let key = if !pid.is_empty() {
                pid
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
            NavDestination::PerformerDetails { name: key }
        }
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
            search_results_filter,
            genre_query,
            studio_query,
            performer_query,
            performer_search_query,
            performer_letter,
            performers_photo_only,
            drilldown,
        } => {
            ui.set_active_screen("browse".into());
            // The active-category change clears applied query in Slint, so
            // restore this category before restoring its saved query below.
            ui.set_active_category(category.clone().into());
            ui.set_search_query(search_query.clone().into());
            ui.set_search_applied_query(search_query.clone().into());
            ui.set_search_results_filter(search_results_filter.clone().into());
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
                if ui.get_search_results_filter().as_str() != search_results_filter.as_str() {
                    ui.invoke_filter_search_results(search_results_filter.into());
                }
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
    let resolver = ctx.resolver.clone();
    let reporter = ctx.reporter.clone();
    let session_ctrl = ctx.session_ctrl.clone();
    let current_item = ctx.current_playing_item.clone();
    let cached_catalog = ctx.cached_catalog.clone();
    let phash_coord = ctx.phash_coord.clone();
    refresh_recent_searches_for_context(ui, &local_store);

    // 1. Screen switching
    {
        let weak = ui.as_weak();
        let player = player.clone();
        let local_store = local_store.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let ctx_switch = ctx.clone();
        ui.on_switch_screen(move |screen| {
            let Some(ui) = weak.upgrade() else { return };
            let current_screen = ui.get_active_screen();
            if current_screen != screen {
                if current_screen == "browse" {
                    ui.invoke_cache_catalog_view();
                }
                ctx_switch.push_nav_destination(current_nav_destination(&ui, &ctx_switch));
            }
            if matches!(current_screen.as_str(), "home" | "browse" | "channels") {
                ui.set_last_content_screen(current_screen);
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
                if !ui.get_is_catalog_search_results() {
                    ui.invoke_filter_category(ui.get_active_category());
                }
            } else if is_settings {
                ui.invoke_refresh_home_categories();
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
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let reporter = reporter.clone();
        let phash_coord = phash_coord.clone();
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
                    let normalized = service_url.normalized;
                    let weak = weak.clone();
                    let credential_store = credential_store.clone();
                    let reporter = reporter.clone();
                    let phash_coord = phash_coord.clone();
                    ui.set_status("Connecting to server...".into());
                    ui.set_status_color(slint::Color::from_rgb_u8(154, 160, 180));
                    std::thread::spawn(move || {
                        match AdultHubClient::new(&normalized)
                            .and_then(|client| client.login(&username, &password))
                        {
                            Ok(auth) => {
                                let session = Session {
                                    server_url: normalized.clone(),
                                    user_id: auth.user.id.clone(),
                                    username: auth.user.username.clone(),
                                    access_token: auth.access_token.clone(),
                                    server_name: "Adult Hub".into(),
                                    refresh_token: Some(auth.refresh_token.clone()),
                                };
                                let session_save = credential_store.save(session.clone());
                                reporter.set_auth(
                                    Some(session.server_url.clone()),
                                    Some(session.access_token.clone()),
                                );
                                crate::thumbnails::set_resolver_base_url(&session.server_url);
                                crate::thumbnails::set_resolver_auth_token(Some(&session.access_token));
                                phash_coord.update_auth(
                                    &session.server_url,
                                    Some(&session.access_token),
                                );
                                slint::invoke_from_event_loop(move || {
                                    let Some(ui) = weak.upgrade() else { return };
                                    ui.set_is_logged_in(true);
                                    ui.set_login_modal_visible(false);
                                    ui.set_login_modal_message("".into());
                                    ui.set_logged_in_user(session.username.into());
                                    ui.set_server_url_input(session.server_url.into());
                                    ui.invoke_filter_category("Latest".into());
                                    ui.set_active_screen("home".into());
                                    ui.invoke_refresh_home();
                                    let warning = session_save.err().map(|_| {
                                        "Could not save this session securely. You can use it until the app closes, then sign in again.".to_owned()
                                    });
                                    if let Some(message) = warning {
                                        ui.set_status(message.clone().into());
                                        ui.set_status_color(slint::Color::from_rgb_u8(255, 190, 90));
                                        ui.set_toast_message(message.into());
                                        ui.set_toast_type("warning".into());
                                    } else {
                                        ui.set_status("Logged in successfully".into());
                                        ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                                        ui.set_toast_message("Connected to Adult Hub successfully!".into());
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
            clear_home_categories_cache();
            phash_coord.update_auth("", None);
            home_catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
            let raw_items = Vec::new();
            let cards = raw_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
            *cached_catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = raw_items;
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

    // 5. Search Catalog
    let catalog_generation = Arc::new(AtomicU64::new(0));
    let search_debounce_generation = Arc::new(AtomicU64::new(0));
    let performer_search_generation = Arc::new(AtomicU64::new(0));
    let catalog_paging = Arc::new(Mutex::new(CatalogPageState::default()));
    let catalog_views = Rc::new(RefCell::new(std::collections::HashMap::<
        String,
        CatalogViewSnapshot,
    >::new()));
    let catalog_filters = Arc::new(Mutex::new(load_catalog_filters(&local_store)));
    let catalog_filter_options = Arc::new(Mutex::new(Vec::<CatalogFilterChoice>::new()));
    {
        let weak = ui.as_weak();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let home_catalog = ctx.home_catalog.clone();
        let weak_for_toggle = weak.clone();
        ui.on_toggle_home_random_filter(move || {
            let Some(ui) = weak_for_toggle.upgrade() else {
                return;
            };
            ui.set_home_random_unwatched_only(!ui.get_home_random_unwatched_only());
            ui.invoke_refresh_home();
        });
        let weak_for_categories = weak.clone();
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
            let local_store = local_store_for_categories.clone();
            std::thread::spawn(move || {
                let categories = match AdultHubClient::new(&session.server_url)
                    .map(|client| client.with_token(&session.access_token))
                {
                    Ok(client) => cached_home_categories(&client, &session).unwrap_or_default(),
                    Err(_) => Vec::new(),
                };
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
        let catalog_views_for_home = catalog_views.clone();
        let ctx_home = ctx.clone();
        ui.on_refresh_home(move || {
            let Some(ui) = weak.upgrade() else { return };
            catalog_views_for_home.borrow_mut().clear();
            let _ = local_store.remove_setting("home_shelves_cache_v1");
            ui.set_home_is_loading(true);
            ui.set_home_error_message(String::new().into());
            ui.set_home_resume_error(String::new().into());
            ui.set_home_latest_error(String::new().into());
            ui.set_home_random_error(String::new().into());
            ui.set_home_performer_error(String::new().into());
            let random_unwatched_only = ui.get_home_random_unwatched_only();
            let weak = weak.clone();
            let credential_store = credential_store.clone();
            let local_store = local_store.clone();
            let home_catalog = home_catalog.clone();
            let ctx_home = ctx_home.clone();

            std::thread::spawn(move || {
                let Some(session) = credential_store.get().ok().flatten() else {
                    home_catalog
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clear();
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

                let client_res = adult_hub_client::AdultHubClient::new(&session.server_url)
                    .map(|c| c.with_token(&session.access_token));

                let client = match client_res {
                    Ok(c) => c,
                    Err(err) => {
                        let is_unauth = err.is_unauthorized();
                        let msg = format!("Failed to initialize Adult Hub client: {err}");
                        slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.set_home_is_loading(false);
                                ui.set_home_error_message(msg.clone().into());
                                ui.set_home_latest_error(msg.into());
                                if is_unauth {
                                    ui.set_is_logged_in(false);
                                    ui.set_login_modal_visible(true);
                                    ui.set_login_modal_message(
                                        "Session expired (401 Unauthorized). Please sign in again."
                                            .into(),
                                    );
                                    ui.set_toast_message(
                                        "Session expired. Please reconnect.".into(),
                                    );
                                    ui.set_toast_type("warning".into());
                                    ui.set_toast_action_text("Sign In".into());
                                    ui.set_toast_visible(true);
                                }
                            }
                        })
                        .ok();
                        return;
                    }
                };

                // Fetch the independent Home shelves concurrently; a slow
                // performers or taxonomy request must not delay Recently Added.
                let (latest_res, random_res, performer_res, categories_result) =
                    std::thread::scope(|scope| {
                        let latest_task = scope.spawn(|| {
                            client.get_catalog_videos(&adult_hub_client::CatalogVideosQuery {
                                offset: Some(0),
                                limit: Some(16),
                                sort: Some("latest"),
                                ..Default::default()
                            })
                        });
                        let random_task = scope.spawn(|| {
                            client.get_catalog_videos(&adult_hub_client::CatalogVideosQuery {
                                offset: Some(0),
                                limit: Some(50),
                                sort: Some("latest"),
                                ..Default::default()
                            })
                        });
                        let performer_task = scope.spawn(|| {
                            client.get_performers(&adult_hub_client::CatalogPerformersQuery {
                                offset: Some(0),
                                limit: Some(100),
                                q: None,
                                letter: None,
                                photo_only: Some(true),
                                sort: Some("video_count"),
                                direction: Some("desc"),
                            })
                        });
                        let categories_task =
                            scope.spawn(|| cached_home_categories(&client, &session));
                        let task_error = || adult_hub_client::AdultHubError::Api {
                            status: 0,
                            message: "home shelf worker panicked".into(),
                            code: None,
                        };
                        (
                            latest_task.join().unwrap_or_else(|_| Err(task_error())),
                            random_task.join().unwrap_or_else(|_| Err(task_error())),
                            performer_task.join().unwrap_or_else(|_| Err(task_error())),
                            categories_task
                                .join()
                                .unwrap_or_else(|_| Err("category worker panicked".into())),
                        )
                    });

                let latest_items = match latest_res {
                    Ok(resp) => resp.items.into_iter().map(TvItem::from).collect::<Vec<_>>(),
                    Err(err) => {
                        let is_unauth = err.is_unauthorized();
                        if is_unauth {
                            // Access tokens expire routinely; try the refresh
                            // token before forcing the user back to the login
                            // screen.
                            if crate::download::refresh_adult_hub_session_if_current(
                                &ctx_home,
                                &session.access_token,
                            )
                            .is_ok()
                            {
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = weak.upgrade() {
                                        ui.invoke_refresh_home();
                                    }
                                });
                                return;
                            }
                        }
                        let msg = format!("Failed to load latest videos: {err}");
                        slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.set_home_is_loading(false);
                                ui.set_home_error_message(msg.clone().into());
                                ui.set_home_latest_error(msg.into());
                                if is_unauth {
                                    ui.set_is_logged_in(false);
                                    ui.set_login_modal_visible(true);
                                    ui.set_login_modal_message(
                                        "Session expired (401 Unauthorized). Please sign in again."
                                            .into(),
                                    );
                                    ui.set_toast_message(
                                        "Session expired. Please reconnect.".into(),
                                    );
                                    ui.set_toast_type("warning".into());
                                    ui.set_toast_action_text("Sign In".into());
                                    ui.set_toast_visible(true);
                                }
                            }
                        })
                        .ok();
                        return;
                    }
                };

                let (mut random_items, random_error) = match random_res {
                    Ok(resp) => (
                        resp.items.into_iter().map(TvItem::from).collect::<Vec<_>>(),
                        String::new(),
                    ),
                    Err(err) => (Vec::new(), format!("Couldn't load Random Picks: {err}")),
                };
                if random_unwatched_only {
                    random_items
                        .retain(|item| !item.is_played && item.playback_position_ticks == 0);
                }
                fastrand::shuffle(&mut random_items);
                random_items.truncate(4);

                let (mut performers, performer_error) = match performer_res {
                    Ok(resp) => (
                        resp.items.into_iter().map(TvItem::from).collect::<Vec<_>>(),
                        String::new(),
                    ),
                    Err(err) => (Vec::new(), format!("Couldn't load Performers: {err}")),
                };
                performers.retain(|person| {
                    person
                        .primary_image_tag
                        .as_ref()
                        .is_some_and(|tag| !tag.trim().is_empty())
                });
                fastrand::shuffle(&mut performers);
                performers.truncate(12);

                let (categories, categories_error) = match categories_result {
                    Ok(categories) => (categories, String::new()),
                    Err(err) => (Vec::new(), format!("Couldn't load categories: {err}")),
                };
                let selected_categories = resolve_home_category_ids(&categories, &local_store);
                let mut category_items = Vec::new();
                let mut category_shelves = Vec::new();
                // Category shelves are independent; fetch them concurrently so
                // three slow requests do not add up before Home can render.
                let selected_shelves = categories
                    .iter()
                    .filter(|category| selected_categories.iter().any(|id| id == &category.id))
                    .collect::<Vec<_>>();
                let fetched_shelves = std::thread::scope(|scope| {
                    let client_ref = &client;
                    let handles = selected_shelves
                        .iter()
                        .map(|category| {
                            scope.spawn(move || {
                                let result =
                                    client_ref.get_videos(&adult_hub_client::CatalogVideosQuery {
                                        offset: Some(0),
                                        limit: Some(16),
                                        sort: Some("latest"),
                                        category: Some(category.title.as_str()),
                                        ..Default::default()
                                    });
                                (category.title.clone(), result)
                            })
                        })
                        .collect::<Vec<_>>();
                    handles
                        .into_iter()
                        .filter_map(|handle| handle.join().ok())
                        .collect::<Vec<_>>()
                });
                for (title, result) in fetched_shelves {
                    let (items, error) = match result {
                        Ok(response) => (
                            response
                                .items
                                .into_iter()
                                .map(TvItem::from)
                                .collect::<Vec<_>>(),
                            String::new(),
                        ),
                        Err(err) => (Vec::new(), format!("Couldn't load {title}: {err}")),
                    };
                    let items = filter_catalog_items(items, &local_store);
                    category_items.extend(items.iter().cloned());
                    category_shelves.push((
                        title,
                        items.into_iter().take(4).collect::<Vec<_>>(),
                        error,
                    ));
                }

                let mut resume = Vec::new();
                if let Ok(records) = local_store.get_all_telegram_progress(50) {
                    for record in records.into_iter().filter(|record| {
                        !record.is_played
                            && record.position_ms > 15_000
                            && (record.duration_ms == 0
                                || record.position_ms < record.duration_ms * 9 / 10)
                    }) {
                        let Some(item_id) = resume_item_id(&record.provider_video_id) else {
                            continue;
                        };
                        resume.push(TvItem {
                            id: item_id,
                            title: if record.title.is_empty() {
                                format!("Telegram {}", record.provider_video_id)
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
                            source_provider: Some("telegram".into()),
                            ..Default::default()
                        });
                    }
                }

                // Local rows only carry what was recorded during playback, and
                // rows written before the metadata arrived have an empty title
                // (the shelf then shows a raw provider reference). The resume id
                // is canonical now, so borrow the metadata of the catalog entry
                // it points at whenever that entry is already loaded.
                let catalog_meta: std::collections::HashMap<&str, &TvItem> = latest_items
                    .iter()
                    .chain(random_items.iter())
                    .chain(performers.iter())
                    .chain(category_items.iter())
                    .map(|item| (item.id.as_str(), item))
                    .collect();
                for entry in resume.iter_mut() {
                    if let Some(meta) = catalog_meta.get(entry.id.as_str()) {
                        entry.title = meta.title.clone();
                        entry.primary_image_tag = meta.primary_image_tag.clone();
                        entry.runtime_ticks = entry.runtime_ticks.or(meta.runtime_ticks);
                        entry.source_provider = meta.source_provider.clone();
                    }
                }

                // Older local rows may not have a title at all. Hydrate the
                // visible resume cards directly by canonical id instead of
                // showing a raw Telegram reference. Keep the lookup bounded
                // and parallel so a slow link cannot stall Home.
                hydrate_missing_titles(&client, &mut resume, 12);
                resume.retain(|item| !has_unresolved_raw_title(item));

                let mut history = local_store
                    .get_watch_history(16)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|json| serde_json::from_str::<TvItem>(&json).ok())
                    .collect::<Vec<_>>();
                hydrate_missing_titles(&client, &mut history, 12);
                history.retain(|item| !has_unresolved_raw_title(item));

                // Try to map legacy hash ids to canonical catalog items. If a
                // title no longer exists in the catalog, hide the stale shelf
                // entry instead of letting playback fail with a 404 later.
                let mut legacy_checks = 0usize;
                let mut stale_ids = std::collections::HashSet::new();
                for item in resume.iter_mut().chain(history.iter_mut()) {
                    if !is_legacy_telegram_hash_id(&item.id) {
                        continue;
                    }
                    if legacy_checks >= 12 {
                        break;
                    }
                    legacy_checks += 1;
                    let stale_id = item.id.clone();
                    if crate::playback::remap_stale_catalog_item(&client, item, &local_store)
                        .is_none()
                    {
                        stale_ids.insert(stale_id);
                    }
                }
                resume.retain(|item| !stale_ids.contains(&item.id));
                history.retain(|item| !stale_ids.contains(&item.id));

                let mut all_home_items = Vec::new();
                all_home_items.extend(resume.clone());
                all_home_items.extend(history.clone());
                all_home_items.extend(latest_items.clone());
                all_home_items.extend(random_items.clone());
                all_home_items.extend(performers.clone());
                all_home_items.extend(category_items);

                *home_catalog
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = all_home_items;

                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        let resume_cards = resume.iter().map(tv_item_to_card).collect::<Vec<_>>();
                        let history_cards = history.iter().map(tv_item_to_card).collect::<Vec<_>>();
                        let latest_cards =
                            latest_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
                        let random_cards =
                            random_items.iter().map(tv_item_to_card).collect::<Vec<_>>();
                        let performer_cards =
                            performers.iter().map(tv_item_to_card).collect::<Vec<_>>();
                        let category_cards = category_shelves
                            .iter()
                            .map(|(name, items, error)| {
                                (
                                    name.clone(),
                                    items
                                        .iter()
                                        .map(tv_item_to_card)
                                        .collect::<Vec<VideoCardData>>(),
                                    error.clone(),
                                )
                            })
                            .collect::<Vec<_>>();

                        ui.set_home_is_loading(false);
                        ui.set_home_error_message(categories_error.into());
                        ui.set_home_resume_error(String::new().into());
                        ui.set_home_latest_error(String::new().into());
                        ui.set_home_random_error(random_error.into());
                        ui.set_home_performer_error(performer_error.into());
                        set_home_shelves(
                            &ui,
                            &[
                                resume_cards,
                                history_cards,
                                latest_cards,
                                random_cards,
                                performer_cards,
                            ],
                        );
                        set_home_category_options(&ui, &categories, &selected_categories);
                        let ui_category_shelves = category_cards
                            .into_iter()
                            .map(|(name, items, error)| HomeCategoryShelfData {
                                name: name.into(),
                                items: Rc::new(slint::VecModel::from(items)).into(),
                                error_message: error.into(),
                            })
                            .collect::<Vec<_>>();
                        ui.set_home_category_shelves(
                            Rc::new(slint::VecModel::from(ui_category_shelves)).into(),
                        );
                        crate::catalog::refresh_home_thumbnails_async(ui.as_weak(), None);
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
                    let (next, descending) = next_catalog_sort(value.sort);
                    value.sort = next;
                    value.descending = descending;
                }
                "direction" => {
                    value.descending = !value.descending;
                }
                "unwatched" => value.unwatched = !value.unwatched,
                "favorites" => value.favorites_only = !value.favorites_only,
                "quality-low" => value.quality = value.quality.toggled(QualityTier::Low),
                "quality-medium" => value.quality = value.quality.toggled(QualityTier::Medium),
                "quality-high" => value.quality = value.quality.toggled(QualityTier::High),
                "quality-clear" => value.quality = QualityTier::All,
                "performer-sort-az" => value.performer_sort = PerformerSort::NameAsc,
                "performer-sort-za" => value.performer_sort = PerformerSort::NameDesc,
                "performer-sort-videos" => value.performer_sort = PerformerSort::VideoCount,
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
        let credential_store = credential_store.clone();
        let catalog_filters = catalog_filters.clone();
        let catalog_filter_options = catalog_filter_options.clone();
        ui.on_open_catalog_filter_picker(move |kind| {
            let Some(ui) = weak.upgrade() else { return };
            let kind = kind.to_string();
            let Some(session) = credential_store.get().ok().flatten() else {
                catalog_filter_options
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clear();
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
            catalog_filter_options
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
            ui.set_catalog_filter_options(
                Rc::new(slint::VecModel::from(Vec::<HomeCategoryOption>::new())).into(),
            );

            let weak = weak.clone();
            let catalog_filter_options = catalog_filter_options.clone();
            let kind_for_worker = kind.clone();
            std::thread::spawn(move || {
                let result = match AdultHubClient::new(&session.server_url) {
                    Ok(client) => {
                        let client = client.with_token(&session.access_token);
                        let taxonomy_query = CatalogTaxonomyQuery {
                            offset: Some(0),
                            limit: Some(500),
                            q: None,
                        };
                        match kind_for_worker.as_str() {
                            "genre" => client.get_categories(&taxonomy_query).map(|response| {
                                response
                                    .items
                                    .into_iter()
                                    .map(|item| item.to_category_tv_item())
                                    .collect::<Vec<_>>()
                            }),
                            "studio" => client.get_studios(&taxonomy_query).map(|response| {
                                response
                                    .items
                                    .into_iter()
                                    .map(|item| item.to_studio_tv_item())
                                    .collect::<Vec<_>>()
                            }),
                            _ => client
                                .get_performers(&adult_hub_client::CatalogPerformersQuery {
                                    offset: Some(0),
                                    limit: Some(500),
                                    q: None,
                                    letter: None,
                                    photo_only: None,
                                    sort: Some("name"),
                                    direction: None,
                                })
                                .map(|response| {
                                    response
                                        .items
                                        .into_iter()
                                        .map(TvItem::from)
                                        .collect::<Vec<_>>()
                                }),
                        }
                        .map_err(|error| error.to_string())
                    }
                    Err(error) => Err(error.to_string()),
                };
                let (options, error) = match result {
                    Ok(items) => {
                        let mut seen = std::collections::HashSet::new();
                        let options = items
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
                        format!("Couldn't load choices from Adult Hub: {error}"),
                    ),
                };
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        if ui.get_catalog_filter_picker_kind().as_str() != kind {
                            return;
                        }
                        *catalog_filter_options
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = options.clone();
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
            let options = catalog_filter_options
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
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
                ui.set_status("Sign in to pick a random catalog title".into());
                return;
            };

            let filters = get_category_filters(&catalog_filters, &category);
            let request_id = catalog_generation.load(Ordering::SeqCst);
            ui.set_catalog_surprise_loading(true);
            ui.set_status("Finding a random title…".into());

            let weak = weak.clone();
            let local_store = local_store.clone();
            let cached_catalog = cached_catalog.clone();
            let catalog_generation = catalog_generation.clone();
            let category = category.clone();
            let requested_category = category.clone();
            std::thread::spawn(move || {
                let result = AdultHubClient::new(&session.server_url)
                    .map(|client| client.with_token(&session.access_token))
                    .and_then(|client| {
                        client.get_videos(&CatalogVideosQuery {
                            offset: Some(0),
                            limit: Some(50),
                            sort: Some("latest"),
                            is_favorite: (category == "Favorites" || filters.favorites_only)
                                .then_some(true),
                            category: (!filters.genre.is_empty()).then_some(filters.genre.as_str()),
                            studio: (!filters.studio.is_empty()).then_some(filters.studio.as_str()),
                            performer: (!filters.performer.is_empty())
                                .then_some(filters.performer.as_str()),
                            quality: filters.quality.query(),
                            ..Default::default()
                        })
                    });
                let network_error = result.as_ref().err().map(ToString::to_string);
                let mut candidates = result
                    .ok()
                    .map(|response| {
                        response
                            .items
                            .into_iter()
                            .map(TvItem::from)
                            .collect::<Vec<_>>()
                    })
                    .map(|items| filter_catalog_items(items, &local_store))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|item| {
                        (category != "Favorites" || item.is_favorite) && filters.accepts(item)
                    })
                    .collect::<Vec<_>>();
                let target = if candidates.is_empty() {
                    None
                } else {
                    let index = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as usize
                        % candidates.len();
                    Some(candidates.swap_remove(index))
                }
                .or_else(|| {
                    let mut fallback = filter_catalog_items(
                        cached_catalog
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone(),
                        &local_store,
                    )
                    .into_iter()
                    .filter(|item| {
                        !item.genres.iter().any(|tag| {
                            tag == "__adult_hub_performer" || tag == "__adult_hub_channel_search"
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
            let leave_drilldown =
                key.as_str() == "genre" && text.is_empty() && ui.get_catalog_category_drilldown();
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
            if leave_drilldown {
                ui.set_catalog_category_drilldown(false);
                ui.set_catalog_genre_query("".into());
            }
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
            let query = query.trim().to_string();
            let current_cat = ui.get_active_category().to_string();
            let is_category_drilldown = ui.get_catalog_category_drilldown();

            // When searching within specialized catalog screens (Category Drilldown,
            // Studios, Categories, Favorites, Watch History, Continue Watching, or Performers),
            // scope the search directly to that screen rather than kicking off a global multi-source search.
            if is_category_drilldown {
                ui.set_search_query(query.clone().into());
                ui.set_search_applied_query(query.clone().into());
                let search_context = active_search_context(&ui);
                if !query.is_empty() {
                    let _ = local_store.add_search_query_context(&search_context, &query);
                }
                refresh_recent_searches_for_context(&ui, &local_store);
                ui.invoke_filter_category("Latest".into());
                return;
            }
            if current_cat == "Studios"
                || current_cat == "Categories"
                || current_cat == "Favorites"
                || current_cat == "Watch History"
                || current_cat == "Continue Watching"
                || current_cat == "Resume"
            {
                ui.set_search_query(query.clone().into());
                ui.set_search_applied_query(query.clone().into());
                let search_context = active_search_context(&ui);
                if !query.is_empty() {
                    let _ = local_store.add_search_query_context(&search_context, &query);
                }
                refresh_recent_searches_for_context(&ui, &local_store);
                ui.invoke_filter_category(current_cat.into());
                return;
            }
            if current_cat == "Performers" {
                ui.set_performer_search_query(query.clone().into());
                ui.set_search_applied_query(query.clone().into());
                if !query.is_empty() {
                    let _ = local_store.add_search_query_context("performers", &query);
                }
                refresh_recent_searches_for_context(&ui, &local_store);
                ui.invoke_filter_category("Performers".into());
                return;
            }

            save_catalog_view(&ui, &catalog_paging, &cached_catalog, &catalog_views);
            ui.set_search_query(query.clone().into());
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
                is_search: !query.is_empty(),
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
            ui.set_search_channels_loading(!query.is_empty());
            ui.set_search_channel_error("".into());
            ui.set_focused_card_index(-1);
            let search_context = active_search_context(&ui);
            let debounce_id = search_debounce_generation.fetch_add(1, Ordering::SeqCst) + 1;
            let weak = weak.clone();
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
                    let _ = local_store.add_search_query_context(&search_context, &query);
                }
                let recent_queries = local_store
                    .get_search_history_context(&search_context, 8)
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
                          is_unauth: bool,
                          channels_loading: bool,
                          channel_error: String,
                          preserve_scroll: bool| {
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
                            *catalog_paging
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                CatalogPageState {
                                    category: "Latest".into(),
                                    is_search: !query.is_empty(),
                                    query,
                                    drilldown: false,
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
                            *cached_catalog
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                filtered.clone();
                            let cards = filtered
                                .iter()
                                .map(|item| {
                                    image_server.as_ref().map_or_else(
                                        || tv_item_to_card(item),
                                        |server| tv_item_to_card_for_server(item, server),
                                    )
                                })
                                .collect::<Vec<_>>();
                            if preserve_scroll {
                                crate::catalog::set_catalog_preserving_scroll(&ui, cards.clone());
                            } else {
                                set_catalog(&ui, cards.clone());
                            }
                            if ui.get_search_results_filter() == "all" {
                                set_search_sections(&ui, &cards);
                            }
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
                            ui.set_search_channels_loading(channels_loading);
                            ui.set_search_channel_error(channel_error.into());
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
                        "Sign in to Adult Hub before searching the catalog.".into(),
                        false,
                        false,
                        String::new(),
                        false,
                    );
                    return;
                };

                let resolver_url = resolver_base_url_for(&local_store, &session);
                let query_str = query.clone();

                // Spawn channel search in background (Stage B)
                let channel_handle = {
                    let resolver = resolver.clone();
                    let resolver_url = resolver_url.clone();
                    let q = query_str.clone();
                    let clean_term = q
                        .trim()
                        .strip_prefix("telegram:")
                        .unwrap_or(q.trim())
                        .to_string();
                    std::thread::spawn(move || {
                        resolver
                            .search_with_timeout(
                                &resolver_url,
                                &clean_term,
                                "telegram",
                                CHANNEL_SEARCH_TIMEOUT,
                            )
                            .map(|videos| videos.into_iter().map(TvItem::from).collect::<Vec<_>>())
                            .map_err(|e| {
                                crate::diagnostics::GLOBAL_LOG.w(
                                    "Search",
                                    &format!("Resolver channel search failed: {:?}", e),
                                );
                                e
                            })
                    })
                };

                // Stage A: query the catalog videos and taxonomy sources.
                let client = match AdultHubClient::new(&session.server_url) {
                    Ok(client) => client.with_token(&session.access_token),
                    Err(error) => {
                        publish(
                            Vec::new(),
                            0,
                            SearchTypeTotals::default(),
                            false,
                            0,
                            format!("Could not initialize Adult Hub client: {error}"),
                            false,
                            false,
                            String::new(),
                            false,
                        );
                        return;
                    }
                };
                let stage_a_res = get_search_movies_page_at(&client, &query_str, &filters, 0);
                let supplements = fetch_search_supplements(&client, &query_str);
                let people_opt = supplements.people;
                let studio_opt = supplements.studios;
                let tag_opt = supplements.tags;

                if search_debounce_generation.load(Ordering::SeqCst) != debounce_id {
                    return;
                }

                let movie_page = match stage_a_res {
                    Ok(page) => page,
                    Err(error) => {
                        let is_unauth = error.is_unauthorized();
                        let message: String = if is_unauth {
                            "Your Adult Hub session expired. Sign in again and retry."
                        } else {
                            "Could not reach Adult Hub. Check the connection and retry."
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
                                false,
                                String::new(),
                                false,
                            );
                            return;
                        }
                        CatalogPage {
                            items: Vec::new(),
                            total_count: 0,
                            has_more: false,
                            next_start: 0,
                            supplemental_total: 0,
                        }
                    }
                };

                let supplements = SearchSupplements {
                    people: people_opt,
                    studios: studio_opt,
                    tags: tag_opt,
                    channels: None,
                };
                let mut totals = supplements.totals();
                totals.movie = movie_page.total_count;

                let mut stage_a_merged = TvItemsPage {
                    items: movie_page.items.clone(),
                    total_record_count: movie_page.total_count,
                    start_index: 0,
                };
                let mut supplemental_total =
                    merge_search_supplements(&mut stage_a_merged, supplements, &local_store);

                // Publish Stage A immediately: Performers, Studios, Movies, Tags appear simultaneously,
                // and Channels shows the compact loading spinner at the bottom
                publish(
                    stage_a_merged.items.clone(),
                    movie_page.next_start,
                    totals,
                    movie_page.has_more,
                    supplemental_total,
                    String::new(),
                    false,
                    true,
                    String::new(),
                    false,
                );

                // Stage B: Wait for channel videos from Resolver
                let channel_res =
                    channel_handle
                        .join()
                        .unwrap_or(Err(adult_hub_client::ResolverError::Api(
                            "Thread panicked".into(),
                        )));
                if search_debounce_generation.load(Ordering::SeqCst) != debounce_id {
                    return;
                }

                match channel_res {
                    Ok(channel_items) => {
                        let channel_supplements = SearchSupplements {
                            people: None,
                            studios: None,
                            tags: None,
                            channels: Some(channel_items),
                        };
                        let channel_added = merge_search_supplements(
                            &mut stage_a_merged,
                            channel_supplements,
                            &local_store,
                        );
                        supplemental_total += channel_added;
                        totals.channel = channel_added;

                        publish(
                            stage_a_merged.items,
                            movie_page.next_start,
                            totals,
                            movie_page.has_more,
                            supplemental_total,
                            String::new(),
                            false,
                            false,
                            String::new(),
                            true,
                        );
                    }
                    Err(_) => {
                        publish(
                            stage_a_merged.items,
                            movie_page.next_start,
                            totals,
                            movie_page.has_more,
                            supplemental_total,
                            String::new(),
                            false,
                            false,
                            "Failed to load channel videos".into(),
                            true,
                        );
                    }
                }
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

            let items = cached_catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
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
            let mut paging = catalog_paging
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let filter_category = {
        let weak = ui.as_weak();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let cached_catalog = cached_catalog.clone();
        let catalog_generation = catalog_generation.clone();
        let search_debounce_generation = search_debounce_generation.clone();
        let performer_search_generation = performer_search_generation.clone();
        let catalog_paging = catalog_paging.clone();
        let catalog_views = catalog_views.clone();
        let catalog_filters = catalog_filters.clone();
        let ctx = ctx.clone();
        let filter_category = move |ui: MainWindow,
                                    cat: slint::SharedString,
                                    save_current_view: bool| {
            let normalized = legacy_catalog_category(cat.as_str()).to_string();
            if normalized != cat.as_str() {
                ui.set_active_category(normalized.clone().into());
            }
            let cat = slint::SharedString::from(normalized);
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
            let is_reclick_refresh = cat == "Random"
                || (ui.get_active_screen() == "browse"
                    && ui.get_active_category() == cat
                    && !ui.get_catalog_category_drilldown()
                    && ui.get_search_query().is_empty());
            if is_reclick_refresh {
                catalog_views
                    .borrow_mut()
                    .retain(|k, _| !k.starts_with(cat.as_str()));
            } else if save_current_view {
                save_catalog_view(&ui, &catalog_paging, &cached_catalog, &catalog_views);
            }
            performer_search_generation.fetch_add(1, Ordering::SeqCst);
            search_debounce_generation.fetch_add(1, Ordering::SeqCst);
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
            let query = match cat.as_str() {
                "Performers" => ui.get_performer_search_query().to_string(),
                "Studios" | "Categories" | "Favorites" | "Watch History" | "Continue Watching"
                | "Resume" => ui.get_search_applied_query().to_string(),
                "Latest" if ui.get_catalog_category_drilldown() => {
                    ui.get_search_applied_query().to_string()
                }
                _ => String::new(),
            };
            ui.set_search_applied_query(query.clone().into());
            let search_context = active_search_context(&ui);
            let trimmed_q = query.trim();
            if !trimmed_q.is_empty() {
                let _ = local_store.add_search_query_context(&search_context, trimmed_q);
            }
            refresh_recent_searches_for_context(&ui, &local_store);
            let cat = cat.to_string();
            let mut filters = get_category_filters(&catalog_filters, &cat);
            if cat == "Performers" {
                filters.performer_letter = performer_letter.clone();
                filters.performer_photo_only = performers_photo_only;
                set_category_filters(&catalog_filters, &cat, filters.clone());
            }
            // The map is the single source of truth for every tab; persist it
            // whenever a view is (re)built so the next launch restores it.
            if let Ok(guard) = catalog_filters.lock() {
                save_catalog_filters(&local_store, &guard);
            }
            let library_id = local_store
                .get_setting("selected_library_id")
                .ok()
                .flatten()
                .unwrap_or_default();
            let target_page = CatalogPageState {
                category: cat.clone(),
                query: query.clone(),
                drilldown: cat == "Latest" && ui.get_catalog_category_drilldown(),
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
                "Random" => "Loading random titles…",
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
            let credential_store = credential_store.clone();
            let local_store = local_store.clone();
            let cached_catalog = cached_catalog.clone();
            let catalog_generation = catalog_generation.clone();
            let catalog_paging = catalog_paging.clone();
            let library_id = library_id.clone();
            let drilldown = target_page.drilldown;
            sync_catalog_filters_to_ui(&ui, &cat, &filters);

            std::thread::spawn(move || {
                let started = std::time::Instant::now();
                let cat_label = cat.clone();
                let mut is_unauth = false;
                let mut image_auth = None;
                let mut catalog_error_message: Option<String> = None;
                let (raw_items, next_start, total_count, has_more, supplemental_total) =
                    if let Some(mut session) = credential_store.get().ok().flatten() {
                        image_auth =
                            Some((session.server_url.clone(), session.access_token.clone()));
                        let mut result = get_catalog_page_with_empty_page_fetch(
                            &session,
                            &local_store,
                            &cat,
                            &query,
                            0,
                            filters.clone(),
                            performers_photo_only,
                        );
                        // The access token lives 15 minutes. Refresh it once and
                        // retry before showing the login modal.
                        if result
                            .as_ref()
                            .err()
                            .is_some_and(CatalogError::is_unauthorized)
                            && crate::download::refresh_adult_hub_session_if_current(
                                &ctx,
                                &session.access_token,
                            )
                            .is_ok()
                            && let Some(updated) = credential_store.get().ok().flatten()
                        {
                            session = updated;
                            image_auth =
                                Some((session.server_url.clone(), session.access_token.clone()));
                            result = get_catalog_page_with_empty_page_fetch(
                                &session,
                                &local_store,
                                &cat,
                                &query,
                                0,
                                filters.clone(),
                                performers_photo_only,
                            );
                        }
                        match result {
                            Ok(page) => (
                                page.items,
                                page.next_start,
                                page.total_count,
                                page.has_more,
                                page.supplemental_total,
                            ),
                            Err(error) => {
                                is_unauth = error.is_unauthorized();
                                catalog_error_message = Some(error.to_string());
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
                        &query,
                    )
                } else {
                    raw_items.clone()
                };
                let image_server = image_auth.as_ref().map(|(server, _)| server.clone());
                let token_for_refresh = image_auth.map(|(_, token)| token);
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        if !catalog_generation_is_current(&catalog_generation, request_id) {
                            return;
                        }
                        *cached_catalog
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = raw_items;
                        *catalog_paging
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            CatalogPageState {
                                category: cat,
                                query,
                                drilldown,
                                is_search: false,
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
                        let reported_total = if has_more { total_count } else { cards.len() };
                        ui.set_catalog_total_count(reported_total as i32);
                        ui.set_is_loading_more(false);
                        ui.set_is_loading_catalog(false);
                        crate::log_debug(&format!(
                            "catalog [{cat_label}]: {} card(s), {reported_total} reported total in {} ms",
                            cards.len(),
                            started.elapsed().as_millis()
                        ));
                        if let Some(ref err_msg) = catalog_error_message {
                            ui.set_status(format!("Catalog error: {err_msg}").into());
                            ui.set_status_color(slint::Color::from_rgb_u8(240, 100, 100));
                        } else {
                            ui.set_status("Ready".into());
                            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                        }
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
        };
        Rc::new(filter_category)
    };
    {
        let weak = ui.as_weak();
        let filter_category = filter_category.clone();
        ui.on_filter_category(move |cat| {
            let Some(ui) = weak.upgrade() else { return };
            filter_category(ui, cat, true);
        });
    }
    {
        let weak = ui.as_weak();
        let catalog_views = catalog_views.clone();
        let filter_category = filter_category.clone();
        ui.on_clear_catalog_search(move || {
            let Some(ui) = weak.upgrade() else { return };
            // Clear-search intentionally omits the transitional snapshot for
            // both scoped and global search results; returning to either view
            // refetches its contents after the query has been cleared.
            CatalogSearchState::cleared().apply(&ui);
            let category = ui.get_active_category();
            clear_saved_view_queries(&catalog_views, &category);
            filter_category(ui, category, false);
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
            catalog_paging
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .loading_more = false;
            ui.set_is_loading_more(false);
            ui.set_catalog_more_error("".into());
            let items = cached_catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
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

    // Keep performer name searches responsive while avoiding a server
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
            let mut page_state = catalog_paging
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                    .ok_or_else(|| CatalogError::Api("Sign in to load more items".into()))
                    .and_then(|session| {
                        get_catalog_page_with_empty_page_fetch(
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
                            let is_search = requested_page.is_search;
                            let items = {
                                let mut cached = cached_catalog
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                            *cached_catalog
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = items.clone();
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
                            let requested_query = requested_page.query.clone();
                            *catalog_paging
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                CatalogPageState {
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
                            let mut reported_total = reported_catalog_total(
                                is_search,
                                is_studios,
                                &requested_query,
                                page.has_more,
                                page.total_count + supplemental_total,
                                cards.len(),
                            );
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
                            let mut paging = catalog_paging
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                            log::warn!(target: "catalog", "{message}");
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
                let performer_key = if !target_item.id.is_empty() {
                    target_item.id.clone()
                } else {
                    format!("name:{}", target_item.title)
                };
                ui.invoke_open_performer_profile(performer_key.into());
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

            ctx_play.set_playback_context(
                cached_catalog
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            );
            crate::playback::start_item_playback_with_resume_ctx(&ui, &ctx_play, target_item, None);
        });
    }
}

#[cfg(test)]
mod tests;
