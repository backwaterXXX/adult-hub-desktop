use crate::catalog::{
    FetchedThumbnail, ThumbnailRequest, fetch_thumbnail_batch, filter_catalog_items,
    refresh_catalog_thumbnails_async, sample_catalog_tv_items, set_catalog, set_discovered_servers,
    tv_item_to_card, tv_item_to_card_for_server,
};
use crate::context::AppContext;
use crate::playback::{show_error, start_item_playback};
use crate::{
    DiscoveredServerItem, HomeCategoryOption, HomeCategoryShelfData, MainWindow, VideoCardData,
};
use adult_hub_client::{
    GetItemsQuery, JellyfinClient, JellyfinError, ResolverClient, ServiceUrlPolicy,
    ServiceUrlValidation, TvItem, TvItemsPage, discover_servers,
};
use adult_hub_storage::{LocalStore, Session};
use slint::{ComponentHandle, Model};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CATALOG_PAGE_SIZE: usize = 50;

fn set_recent_searches(ui: &MainWindow, searches: Vec<String>) {
    let items: Vec<slint::SharedString> = searches.into_iter().map(Into::into).collect();
    ui.set_recent_searches(Rc::new(slint::VecModel::from(items)).into());
}

fn search_result_counts(items: &[TvItem]) -> (usize, usize, usize) {
    let mut movies = 0;
    let mut performers = 0;
    let mut channels = 0;
    for item in items {
        if item.genres.iter().any(|tag| tag == "__adult_hub_performer") {
            performers += 1;
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
    (movies, performers, channels)
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

fn update_home_shelf_thumbnails(
    ui: &MainWindow,
    shelf_index: usize,
    thumbnails: &[(usize, FetchedThumbnail)],
) {
    let items = match shelf_index {
        0 => ui.get_home_resume_items(),
        1 => ui.get_home_history_items(),
        2 => ui.get_home_latest_items(),
        3 => ui.get_home_random_items(),
        _ => ui.get_home_performer_items(),
    };
    for (index, thumbnail) in thumbnails {
        let Some(mut card) = items.row_data(*index) else {
            continue;
        };
        let Some(image) = crate::thumbnails::image_from_bytes(&thumbnail.bytes) else {
            continue;
        };
        card.thumbnail_url = thumbnail.url.clone().into();
        card.thumbnail = image;
        card.has_thumbnail = true;
        items.set_row_data(*index, card);
    }
}

fn update_home_category_thumbnails(
    ui: &MainWindow,
    category_index: usize,
    thumbnails: &[(usize, FetchedThumbnail)],
) {
    let categories = ui.get_home_category_shelves();
    if let Some(category) = categories.row_data(category_index) {
        for (index, thumbnail) in thumbnails {
            let Some(mut card) = category.items.row_data(*index) else {
                continue;
            };
            let Some(image) = crate::thumbnails::image_from_bytes(&thumbnail.bytes) else {
                continue;
            };
            card.thumbnail_url = thumbnail.url.clone().into();
            card.thumbnail = image;
            card.has_thumbnail = true;
            category.items.set_row_data(*index, card);
        }
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
                let end = (start + crate::catalog::THUMBNAIL_BATCH_SIZE).min(shelf.len());
                let thumbnails = fetch_thumbnail_batch(&shelf[start..end], Some(&token))
                    .into_iter()
                    .map(|(offset, thumbnail)| (start + offset, thumbnail))
                    .collect::<Vec<_>>();
                let generation = generation.clone();
                let ui = ui.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if generation.load(Ordering::SeqCst) != request_id {
                        return;
                    }
                    if let Some(ui) = ui.upgrade() {
                        update_home_shelf_thumbnails(&ui, shelf_index, &thumbnails);
                    }
                });
            }
        }
        for (category_index, (_, shelf, _)) in category_shelves.iter().enumerate() {
            for start in (0..shelf.len()).step_by(crate::catalog::THUMBNAIL_BATCH_SIZE) {
                let end = (start + crate::catalog::THUMBNAIL_BATCH_SIZE).min(shelf.len());
                let thumbnails = fetch_thumbnail_batch(&shelf[start..end], Some(&token))
                    .into_iter()
                    .map(|(offset, thumbnail)| (start + offset, thumbnail))
                    .collect::<Vec<_>>();
                let generation = generation.clone();
                let ui = ui.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if generation.load(Ordering::SeqCst) != request_id {
                        return;
                    }
                    if let Some(ui) = ui.upgrade() {
                        update_home_category_thumbnails(&ui, category_index, &thumbnails);
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
    next_start: usize,
    has_more: bool,
    loading_more: bool,
    filters: CatalogFilters,
    supplemental_total: usize,
}

#[derive(Clone)]
struct CatalogFilters {
    sort: u8,
    descending: bool,
    unwatched: bool,
    favorites_only: bool,
    hd_only: bool,
    duration: u8,
    genre: String,
    studio: String,
    performer: String,
    performer_letter: String,
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
            studio: String::new(),
            performer: String::new(),
            performer_letter: String::new(),
        }
    }
}

impl CatalogFilters {
    fn sort_by(&self) -> &'static str {
        match self.sort {
            1 => "SortName",
            2 => "Runtime",
            _ => "DateCreated",
        }
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
        if !self.performer.is_empty()
            && !item
                .people
                .iter()
                .any(|person| person.name.eq_ignore_ascii_case(&self.performer))
        {
            return false;
        }
        true
    }
}

fn catalog_filter_labels(filters: &CatalogFilters) -> (String, String) {
    let sort = match filters.sort {
        1 => "Title",
        2 => "Runtime",
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
            history.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
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
                parent_id: selected_library_id.as_deref(),
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
        let movie_query = GetItemsQuery {
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
            ..Default::default()
        };
        let people_query = GetItemsQuery {
            parent_id: selected_library_id.as_deref(),
            start_index: 0,
            limit: 30,
            sort_by: "SortName",
            search_term: Some(query),
            include_item_types: Some("Person"),
            person_types: Some("Actor"),
            ..Default::default()
        };
        let resolver_base_url = local_store
            .get_setting("resolver_base_url")
            .ok()
            .flatten()
            .or_else(|| local_store.get_setting("resolver_url").ok().flatten())
            .unwrap_or_else(|| session.server_url.clone());

        let (movie_result, people_result, channel_result) = std::thread::scope(|scope| {
            let movie_task = scope.spawn(|| {
                jellyfin.get_items(
                    &session.server_url,
                    &session.user_id,
                    &session.access_token,
                    &movie_query,
                )
            });
            let people_task = (start_index == 0).then(|| {
                scope.spawn(|| {
                    jellyfin.get_items(
                        &session.server_url,
                        &session.user_id,
                        &session.access_token,
                        &people_query,
                    )
                })
            });
            let channel_task = (start_index == 0)
                .then(|| scope.spawn(|| resolver.search(&resolver_base_url, query, "telegram")));

            let movie_result = movie_task
                .join()
                .map_err(|_| JellyfinError::Api("Library search worker panicked".into()))?;
            let people_result = people_task
                .map(|task| {
                    task.join()
                        .map_err(|_| JellyfinError::Api("Performer search worker panicked".into()))
                })
                .transpose()?;
            let channel_result = channel_task
                .map(|task| {
                    task.join()
                        .map_err(|_| JellyfinError::Api("Channel search worker panicked".into()))
                })
                .transpose()?;
            Ok::<_, JellyfinError>((movie_result, people_result, channel_result))
        })?;

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
        let people_page = match people_result {
            Some(Ok(page)) => {
                successful_sources += 1;
                Some(page)
            }
            Some(Err(_)) => {
                crate::diagnostics::GLOBAL_LOG.w(
                    "Search",
                    "Jellyfin performer search failed; preserving results from other sources",
                );
                None
            }
            None => None,
        };
        let channel_videos = match channel_result {
            Some(Ok(videos)) => {
                successful_sources += 1;
                Some(videos)
            }
            Some(Err(_)) => {
                crate::diagnostics::GLOBAL_LOG.w(
                    "Search",
                    "Resolver channel search failed; preserving results from other sources",
                );
                None
            }
            None => None,
        };
        if successful_sources == 0 {
            return Err(JellyfinError::Api("All search sources failed".into()));
        }

        if let Some(mut people_page) = people_page {
            supplemental_count = people_page.items.len();
            supplemental_total = supplemental_count;
            for person in &mut people_page.items {
                person.genres.push("__adult_hub_performer".into());
            }
            movie_page.items.extend(people_page.items);
            movie_page.total_record_count += supplemental_total;
        }
        if let Some(videos) = channel_videos {
            let mut channel_items = videos.into_iter().map(TvItem::from).collect::<Vec<_>>();
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
            supplemental_count += channel_count;
            supplemental_total += channel_count;
            for channel_item in &mut channel_items {
                channel_item
                    .genres
                    .push("__adult_hub_channel_search".into());
            }
            movie_page.items.extend(channel_items);
            movie_page.total_record_count += channel_count;
        }
        movie_page
    } else {
        match category {
            "Resume" => jellyfin.get_resume_items_page(
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
            "Performers" | "Studios" | "Categories" => jellyfin.get_items(
                &session.server_url,
                &session.user_id,
                &session.access_token,
                &GetItemsQuery {
                    parent_id: selected_library_id.as_deref(),
                    start_index,
                    limit: CATALOG_PAGE_SIZE,
                    sort_by: "SortName",
                    sort_descending: false,
                    include_item_types: Some(match category {
                        "Performers" => "Person",
                        "Studios" => "Studio",
                        _ => "Genre",
                    }),
                    person_types: (category == "Performers").then_some("Actor"),
                    ..Default::default()
                },
            )?,
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
    if category == "Resume" && query.is_empty() {
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
    } else {
        page.items.retain(|item| {
            item.genres
                .iter()
                .any(|tag| tag == "__adult_hub_performer" || tag == "__adult_hub_channel_search")
                || filters.accepts(item)
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

pub fn install_stage2_handlers(ui: &MainWindow, ctx: &AppContext) {
    let player = ctx.player.clone();
    let headers = ctx.headers.clone();
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
    set_recent_searches(ui, local_store.get_search_history(8).unwrap_or_default());

    // 1. Screen switching
    {
        let weak = ui.as_weak();
        let player = player.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let qc_generation = qc_generation.clone();
        ui.on_switch_screen(move |screen| {
            let Some(ui) = weak.upgrade() else { return };
            let current_screen = ui.get_active_screen();
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
            let is_channels = screen == "channels";
            let is_home = screen == "home";
            let is_settings = screen == "settings";
            if screen == "player" {
                ui.set_player_controls_visible(true);
            }
            ui.set_active_screen(screen);
            if is_channels {
                ui.invoke_select_channel_category(ui.get_active_channel_category());
            } else if is_home {
                ui.invoke_refresh_home();
            } else if is_settings {
                ui.invoke_refresh_home_categories();
                ui.invoke_refresh_library_list();
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
                                let eff_res_url = local_store
                                    .get_setting("resolver_base_url")
                                    .ok()
                                    .flatten()
                                    .or_else(|| {
                                        local_store.get_setting("resolver_url").ok().flatten()
                                    })
                                    .unwrap_or_else(|| session.server_url.clone());
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
                                    let eff_res_url = local_store
                                        .get_setting("resolver_base_url")
                                        .ok()
                                        .flatten()
                                        .or_else(|| {
                                            local_store.get_setting("resolver_url").ok().flatten()
                                        })
                                        .unwrap_or_else(|| session.server_url.clone());
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
    let catalog_filters = Arc::new(Mutex::new(CatalogFilters::default()));
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
                let parent_id = local_store
                    .get_setting("selected_library_id")
                    .ok()
                    .flatten();
                let categories_result = jellyfin.get_items(
                    &session.server_url,
                    &session.user_id,
                    &session.access_token,
                    &GetItemsQuery {
                        parent_id: parent_id.as_deref(),
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
                        parent_id: parent_id.as_deref(),
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
                    }
                }
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
                let shelves = [resume, history, latest, random, performers]
                    .into_iter()
                    .map(|items| items.into_iter().take(4).collect::<Vec<_>>())
                    .collect::<Vec<_>>();
                *home_catalog.lock().unwrap() = all_items;
                let categories_for_ui = categories.clone();
                let selected_categories_for_ui = selected_categories.clone();
                let server_url = session.server_url.clone();
                slint::invoke_from_event_loop(move || {
                    if home_generation.load(Ordering::SeqCst) != request_id {
                        return;
                    }
                    if let Some(ui) = weak.upgrade() {
                        ui.set_home_is_loading(false);
                        ui.set_home_error_message(categories_error.into());
                        ui.set_home_resume_error(resume_error.into());
                        ui.set_home_latest_error(latest_error.into());
                        ui.set_home_random_error(random_error.into());
                        ui.set_home_performer_error(performer_error.into());
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
            let mut value = filters.lock().unwrap();
            match action.as_str() {
                "sort" => value.sort = (value.sort + 1) % 3,
                "direction" => value.descending = !value.descending,
                "unwatched" => value.unwatched = !value.unwatched,
                "favorites" => value.favorites_only = !value.favorites_only,
                "hd" => value.hd_only = !value.hd_only,
                "duration" => value.duration = (value.duration + 1) % 4,
                _ => return,
            }
            let current = value.clone();
            drop(value);
            let (sort, duration) = catalog_filter_labels(&current);
            ui.set_catalog_sort_label(sort.into());
            ui.set_catalog_sort_descending(current.descending);
            ui.set_catalog_unwatched_only(current.unwatched);
            ui.set_catalog_favorites_only(current.favorites_only);
            ui.set_catalog_hd_only(current.hd_only);
            ui.set_catalog_duration_label(duration.into());
            if ui.get_search_query().trim().is_empty() {
                ui.invoke_filter_category(ui.get_active_category());
            } else {
                ui.invoke_search_catalog(ui.get_search_query());
            }
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

            let selected = {
                let filters = catalog_filters.lock().unwrap();
                match kind.as_str() {
                    "genre" => filters.genre.clone(),
                    "studio" => filters.studio.clone(),
                    _ => filters.performer.clone(),
                }
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

            let filters = catalog_filters.lock().unwrap().clone();
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
                    if catalog_generation.load(Ordering::SeqCst) != request_id
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
                let mut current = filters.lock().unwrap();
                current.genre.clear();
                current.studio.clear();
                current.performer.clear();
                drop(current);
                ui.set_catalog_genre_query("".into());
                ui.set_catalog_studio_query("".into());
                ui.set_catalog_performer_query("".into());
            }
        });
    }
    {
        let weak = ui.as_weak();
        let filters = catalog_filters.clone();
        ui.on_set_catalog_text_filter(move |key, value| {
            let Some(ui) = weak.upgrade() else { return };
            let mut current = filters.lock().unwrap();
            let text = value.trim().to_string();
            match key.as_str() {
                "genre" => current.genre = text,
                "studio" => current.studio = text,
                "performer" => current.performer = text,
                _ => return,
            }
            drop(current);
            if ui.get_search_query().trim().is_empty() {
                ui.invoke_filter_category(ui.get_active_category());
            } else {
                ui.invoke_search_catalog(ui.get_search_query());
            }
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
        ui.on_search_catalog(move |query| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_is_loading_catalog(true);
            let query = query.trim().to_string();
            ui.set_is_catalog_search_results(!query.is_empty());
            ui.set_search_applied_query(query.clone().into());
            ui.set_search_error_message("".into());
            ui.set_search_all_has_more(false);
            ui.set_search_all_total_count(0);
            ui.set_search_results_filter("all".into());
            ui.set_search_movie_count(0);
            ui.set_search_performer_count(0);
            ui.set_search_channel_count(0);
            ui.set_focused_card_index(0);
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
            let filters = catalog_filters.lock().unwrap().clone();
            ui.set_is_loading_more(false);
            ui.set_catalog_has_more(false);

            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                if search_debounce_generation.load(Ordering::SeqCst) != debounce_id {
                    return;
                }
                if !query.is_empty() {
                    let _ = local_store.add_search_query(&query);
                }
                let recent_queries = local_store.get_search_history(8).unwrap_or_default();

                let mut is_unauth = false;
                let mut search_error_message = String::new();
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
                            "Latest",
                            &query,
                            0,
                            filters.clone(),
                            false,
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
                                search_error_message = if is_unauth {
                                    "Your Jellyfin session expired. Sign in again and retry."
                                } else {
                                    "Could not reach Jellyfin. Check the connection and retry."
                                }
                                .into();
                                (Vec::new(), 0, 0, false, 0)
                            }
                        }
                    } else {
                        search_error_message =
                            "Connect Jellyfin before searching your library.".into();
                        (Vec::new(), 0, 0, false, 0)
                    };
                let raw_items = filter_catalog_items(raw_items, &local_store);
                let (movie_count, performer_count, channel_count) =
                    search_result_counts(&raw_items);
                let image_server = image_auth.as_ref().map(|(server, _)| server.clone());
                let token_for_refresh = image_auth.map(|(_, token)| token);

                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        if catalog_generation.load(Ordering::SeqCst) != request_id {
                            return;
                        }
                        *catalog_paging.lock().unwrap() = CatalogPageState {
                            category: "Latest".into(),
                            query,
                            next_start,
                            has_more,
                            loading_more: false,
                            filters,
                            supplemental_total,
                        };
                        *cached_catalog.lock().unwrap() = raw_items.clone();
                        let cards = raw_items
                            .iter()
                            .map(|item| {
                                image_server.as_ref().map_or_else(
                                    || tv_item_to_card(item),
                                    |server| tv_item_to_card_for_server(item, server),
                                )
                            })
                            .collect::<Vec<_>>();
                        set_catalog(&ui, cards);
                        set_recent_searches(&ui, recent_queries);
                        ui.set_search_movie_count(movie_count as i32);
                        ui.set_search_performer_count(performer_count as i32);
                        ui.set_search_channel_count(channel_count as i32);
                        ui.set_search_error_message(search_error_message.into());
                        ui.set_search_all_has_more(has_more);
                        ui.set_search_all_total_count(total_count as i32);
                        ui.set_catalog_has_more(has_more);
                        ui.set_catalog_total_count(total_count as i32);
                        ui.set_is_loading_more(false);
                        ui.set_is_loading_catalog(false);
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
                                    || tag == "__adult_hub_channel_search"
                            })
                    }
                    "performers" => item.genres.iter().any(|tag| tag == "__adult_hub_performer"),
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
            paging.has_more = kind == "all" && ui.get_search_all_has_more();
            let has_more = paging.has_more;
            drop(paging);

            ui.set_search_results_filter(kind.clone());
            ui.set_catalog_total_count(if kind == "all" {
                ui.get_search_all_total_count()
            } else {
                filtered.len() as i32
            });
            ui.set_catalog_has_more(has_more);
            ui.set_is_loading_more(false);
            ui.set_focused_card_index(0);
            set_catalog(&ui, cards.clone());

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
        let catalog_filters = catalog_filters.clone();
        ui.on_filter_category(move |cat| {
            let Some(ui) = weak.upgrade() else { return };
            if cat != "Latest" {
                ui.set_catalog_category_drilldown(false);
            }
            performer_search_generation.fetch_add(1, Ordering::SeqCst);
            search_debounce_generation.fetch_add(1, Ordering::SeqCst);
            ui.set_is_catalog_search_results(false);
            ui.set_search_error_message("".into());
            ui.set_search_results_filter("all".into());
            let request_id = catalog_generation.fetch_add(1, Ordering::SeqCst) + 1;
            if cat == "Channels" {
                ui.set_active_screen("channels".into());
                ui.invoke_select_channel_category("All Channels".into());
                return;
            }
            ui.set_is_loading_catalog(true);
            ui.set_is_loading_more(false);
            ui.set_catalog_has_more(false);
            let performer_letter = ui.get_performer_letter().to_string();
            let performers_photo_only = ui.get_performers_photo_only();
            let performer_search_query = if cat == "Performers" {
                ui.get_performer_search_query().to_string()
            } else {
                String::new()
            };
            let cat = cat.to_string();
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let resolver = resolver.clone();
            let credential_store = credential_store.clone();
            let local_store = local_store.clone();
            let cached_catalog = cached_catalog.clone();
            let catalog_generation = catalog_generation.clone();
            let catalog_paging = catalog_paging.clone();
            let mut filters = catalog_filters.lock().unwrap().clone();
            if cat == "Performers" {
                filters.performer_letter = performer_letter.clone();
            }

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
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        if catalog_generation.load(Ordering::SeqCst) != request_id {
                            return;
                        }
                        *cached_catalog.lock().unwrap() = raw_items;
                        *catalog_paging.lock().unwrap() = CatalogPageState {
                            category: cat,
                            query: performer_search_query,
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
                        ui.set_catalog_total_count(total_count as i32);
                        ui.set_is_loading_more(false);
                        ui.set_is_loading_catalog(false);
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
        ui.on_request_performer_search(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_active_category() != "Performers" {
                return;
            }
            let scheduled_query = ui.get_performer_search_query().to_string();
            let request_id = performer_search_generation.fetch_add(1, Ordering::SeqCst) + 1;
            let generation = performer_search_generation.clone();
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
        ui.on_load_more_catalog(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut page_state = catalog_paging.lock().unwrap();
            if !page_state.has_more || page_state.loading_more {
                return;
            }
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
                    if catalog_generation.load(Ordering::SeqCst) != request_id {
                        return;
                    }

                    match result {
                        Ok((page, server_url, token)) => {
                            let supplemental_total = requested_page.supplemental_total;
                            let is_search = !requested_page.query.is_empty();
                            let items = {
                                let mut cached = cached_catalog.lock().unwrap();
                                let mut seen = cached
                                    .iter()
                                    .map(|item| item.id.clone())
                                    .collect::<std::collections::HashSet<_>>();
                                for item in page.items {
                                    if seen.insert(item.id.clone()) {
                                        cached.push(item);
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

                            *catalog_paging.lock().unwrap() = CatalogPageState {
                                next_start: page.next_start,
                                has_more: page.has_more,
                                loading_more: false,
                                ..requested_page
                            };
                            set_catalog(&ui, cards.clone());
                            if is_search {
                                let (movie_count, performer_count, channel_count) =
                                    search_result_counts(&items);
                                ui.set_search_movie_count(movie_count as i32);
                                ui.set_search_performer_count(performer_count as i32);
                                ui.set_search_channel_count(channel_count as i32);
                                ui.set_search_all_has_more(page.has_more);
                                ui.set_search_all_total_count(
                                    (page.total_count + supplemental_total) as i32,
                                );
                            }
                            ui.set_catalog_has_more(page.has_more);
                            ui.set_catalog_total_count(
                                (page.total_count + supplemental_total) as i32,
                            );
                            ui.set_is_loading_more(false);
                            refresh_catalog_thumbnails_async(
                                ui.as_weak(),
                                cards,
                                token,
                                cached_catalog.clone(),
                                Some((catalog_generation.clone(), request_id)),
                            );
                        }
                        Err(error) => {
                            catalog_paging.lock().unwrap().loading_more = false;
                            ui.set_is_loading_more(false);
                            ui.set_status(format!("Could not load more items: {error}").into());
                            ui.set_status_color(slint::Color::from_rgb_u8(255, 118, 118));
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
        let player = player.clone();
        let headers = headers.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let reporter = reporter.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let cached_catalog = cached_catalog.clone();
        let home_catalog = ctx.home_catalog.clone();
        let phash_coord = phash_coord.clone();
        ui.on_play_item(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(player) = &player else {
                show_error(&ui, "libmpv is not available");
                return;
            };
            let id = id.to_string();

            // Locate in cached catalog or synthesize TvItem
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
                })
                .unwrap_or_else(|| {
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
                            width: None,
                            height: None,
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
                            width: None,
                            height: None,
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

            start_item_playback(
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
            );
        });
    }
}
