use crate::{DiscoveredServerItem, MainWindow, VideoCardData, VideoCardRowData};
use adult_hub_client::{JellyfinClient, TvItem, TvPersonRole};
use adult_hub_storage::LocalStore;
use slint::Model;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

pub const THUMBNAIL_BATCH_SIZE: usize = 4;

#[derive(Clone)]
pub struct ThumbnailRequest {
    pub id: String,
    pub primary_url: String,
    pub fallback_url: String,
    pub alternate_url: String,
    pub provider: String,
    pub progress: f32,
}

#[derive(Clone)]
pub struct FetchedThumbnail {
    pub url: String,
    pub bytes: Vec<u8>,
}

struct ThumbnailFlight {
    result: Mutex<Option<Option<FetchedThumbnail>>>,
    completed: Condvar,
}

static THUMBNAIL_FLIGHTS: OnceLock<Mutex<HashMap<String, Arc<ThumbnailFlight>>>> = OnceLock::new();
static THUMBNAIL_DOWNLOADS: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();

struct ThumbnailDownloadPermit;

impl ThumbnailDownloadPermit {
    fn acquire() -> Option<Self> {
        const MAX_CONCURRENT_DOWNLOADS: usize = 4;
        let (active, available) =
            THUMBNAIL_DOWNLOADS.get_or_init(|| (Mutex::new(0), Condvar::new()));
        let mut count = active.lock().ok()?;
        while *count >= MAX_CONCURRENT_DOWNLOADS {
            count = available.wait(count).ok()?;
        }
        *count += 1;
        Some(Self)
    }
}

impl Drop for ThumbnailDownloadPermit {
    fn drop(&mut self) {
        if let Some((active, available)) = THUMBNAIL_DOWNLOADS.get()
            && let Ok(mut count) = active.lock()
        {
            *count = count.saturating_sub(1);
            available.notify_one();
        }
    }
}

fn thumbnail_flight_key(request: &ThumbnailRequest, token: Option<&str>) -> String {
    let mut key_context = ring::digest::Context::new(&ring::digest::SHA256);
    for value in [
        request.id.as_str(),
        request.primary_url.as_str(),
        request.fallback_url.as_str(),
        request.alternate_url.as_str(),
        request.provider.as_str(),
    ] {
        key_context.update(value.as_bytes());
        key_context.update(&[0]);
    }
    key_context.update(&request.progress.to_bits().to_le_bytes());
    if let Some(token) = token {
        let token_digest = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
        key_context.update(token_digest.as_ref());
    }
    key_context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

impl From<&VideoCardData> for ThumbnailRequest {
    fn from(card: &VideoCardData) -> Self {
        Self {
            id: card.id.to_string(),
            primary_url: card.thumbnail_url.to_string(),
            fallback_url: card.fallback_thumbnail_url.to_string(),
            alternate_url: card.alternate_thumbnail_url.to_string(),
            provider: card.provider.to_string(),
            progress: card.progress,
        }
    }
}

pub(crate) fn thumbnail_request_matches_card(
    request: &ThumbnailRequest,
    card: &VideoCardData,
) -> bool {
    if card.id != request.id || card.provider != request.provider {
        return false;
    }
    let mut requested_urls = [
        request.primary_url.as_str(),
        request.fallback_url.as_str(),
        request.alternate_url.as_str(),
    ]
    .into_iter()
    .filter(|url| !url.is_empty())
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let mut current_urls = [
        card.thumbnail_url.as_str(),
        card.fallback_thumbnail_url.as_str(),
        card.alternate_thumbnail_url.as_str(),
    ]
    .into_iter()
    .filter(|url| !url.is_empty())
    .map(str::to_owned)
    .collect::<Vec<_>>();
    requested_urls.sort_unstable();
    requested_urls.dedup();
    current_urls.sort_unstable();
    current_urls.dedup();
    requested_urls == current_urls
}

pub(crate) fn card_index_by_request(
    model: &slint::ModelRc<VideoCardData>,
    request: &ThumbnailRequest,
) -> Option<usize> {
    (0..model.row_count()).find(|index| {
        model
            .row_data(*index)
            .is_some_and(|card| thumbnail_request_matches_card(request, &card))
    })
}

fn thumbnail_generation_is_current(generation: &AtomicU64, request_id: u64) -> bool {
    generation.load(Ordering::SeqCst) == request_id
}

pub(crate) fn no_thumbnail_state(has_source_url: bool) -> &'static str {
    if has_source_url { "failed" } else { "missing" }
}

fn invalidate_thumbnail_request_cache(request: &ThumbnailRequest) {
    for url in [
        request.primary_url.as_str(),
        request.fallback_url.as_str(),
        request.alternate_url.as_str(),
    ] {
        crate::thumbnails::invalidate_cached_image(url);
    }
}

pub fn make_sample_person(name: &str) -> TvPersonRole {
    TvPersonRole {
        id: name.to_lowercase().replace(' ', "_"),
        name: name.to_string(),
        role: Some("Actor".to_string()),
        person_type: Some("Actor".to_string()),
        primary_image_tag: None,
    }
}

pub fn sample_catalog_tv_items() -> Vec<TvItem> {
    vec![
        TvItem {
            id: "sample".into(),
            title: "Local Demo Sample (1080p)".into(),
            overview: Some("Local MP4 test sample".into()),
            year: Some(2026),
            runtime_ticks: Some(100_000_000), // 10s
            genres: vec!["Sample".into()],
            studios: vec!["Local".into()],
            container: Some("mp4".into()),
            file_size_bytes: Some(120_000),
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        },
        TvItem {
            id: "telegram:demo_channel:101".into(),
            title: "Channel Highlight #101".into(),
            overview: Some("Exclusive channel video".into()),
            year: Some(2026),
            runtime_ticks: Some(8_620_000_000), // 14m 22s = 862s
            genres: vec!["Channel".into()],
            studios: vec!["Neon Pulse".into()],
            people: vec![
                make_sample_person("Eva Green"),
                make_sample_person("Mia Ray"),
            ],
            provider_video_id: Some("demo_channel:101".into()),
            container: Some("mp4".into()),
            file_size_bytes: Some(450_000_000),
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        },
        TvItem {
            id: "telegram:demo_channel:102".into(),
            title: "Exclusive Scene #102".into(),
            overview: Some("Scene clip".into()),
            year: Some(2026),
            runtime_ticks: Some(15_400_000_000), // 25m 40s
            genres: vec!["Exclusive".into()],
            studios: vec!["4K Studios".into()],
            people: vec![make_sample_person("Lexi Luna")],
            is_favorite: true,
            is_played: true,
            provider_video_id: Some("demo_channel:102".into()),
            container: Some("mp4".into()),
            file_size_bytes: Some(850_000_000),
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        },
    ]
}

#[cfg(test)]
pub fn sample_catalog_items() -> Vec<VideoCardData> {
    sample_catalog_tv_items()
        .iter()
        .map(tv_item_to_card)
        .collect()
}

#[cfg(test)]
pub fn sample_channel_items() -> Vec<VideoCardData> {
    vec![
        VideoCardData {
            id: "telegram:channel_4k:1".into(),
            title: "4K Studios Channel - Top Clips".into(),
            genre: "".into(),
            duration: "32:15".into(),
            year: "2026".into(),
            is_played: false,
            provider: "telegram".into(),
            is_favorite: false,
            studio: "4K Studios".into(),
            progress: 0.0,
            has_thumbnail: false,
            thumbnail_state: "missing".into(),
            thumbnail: slint::Image::default(),
            card_index: 0,
            thumbnail_url: "".into(),
            fallback_thumbnail_url: "".into(),
            alternate_thumbnail_url: "".into(),
        },
        VideoCardData {
            id: "telegram:channel_vr:2".into(),
            title: "VR Scenes Channel - Volume 3".into(),
            genre: "".into(),
            duration: "45:00".into(),
            year: "2026".into(),
            is_played: false,
            provider: "telegram".into(),
            is_favorite: false,
            studio: "VR Scenes".into(),
            progress: 0.0,
            has_thumbnail: false,
            thumbnail_state: "missing".into(),
            thumbnail: slint::Image::default(),
            card_index: 1,
            thumbnail_url: "".into(),
            fallback_thumbnail_url: "".into(),
            alternate_thumbnail_url: "".into(),
        },
    ]
}

pub fn cards_to_rows(cards: &[VideoCardData], cols_per_row: usize) -> Vec<VideoCardRowData> {
    let mut rows = Vec::new();
    let cols = cols_per_row.clamp(1, 4);
    for chunk in cards.chunks(cols) {
        let mut row = VideoCardRowData {
            has_0: false,
            card_0: chunk.first().cloned().unwrap_or_default(),
            has_1: false,
            card_1: chunk.get(1).cloned().unwrap_or_default(),
            has_2: false,
            card_2: chunk.get(2).cloned().unwrap_or_default(),
            has_3: false,
            card_3: chunk.get(3).cloned().unwrap_or_default(),
        };
        if !chunk.is_empty() {
            row.has_0 = true;
        }
        if chunk.len() > 1 {
            row.has_1 = true;
        }
        if chunk.len() > 2 {
            row.has_2 = true;
        }
        if chunk.len() > 3 {
            row.has_3 = true;
        }
        rows.push(row);
    }
    rows
}

pub fn set_catalog(ui: &MainWindow, items: Vec<VideoCardData>) {
    let mut indexed = items;
    for (i, card) in indexed.iter_mut().enumerate() {
        card.card_index = i as i32;
    }
    let rows = cards_to_rows(&indexed, ui.get_catalog_column_count() as usize);
    let rows_model = Rc::new(slint::VecModel::from(rows));
    ui.set_catalog_rows(rows_model.into());

    let model = Rc::new(slint::VecModel::from(indexed));
    ui.set_catalog_items(model.into());
}

pub fn set_channel_catalog(ui: &MainWindow, items: Vec<VideoCardData>) {
    let mut indexed = items;
    for (i, card) in indexed.iter_mut().enumerate() {
        card.card_index = i as i32;
    }
    let rows = cards_to_rows(&indexed, ui.get_channel_column_count() as usize);
    let rows_model = Rc::new(slint::VecModel::from(rows));
    ui.set_channel_rows(rows_model.into());

    let model = Rc::new(slint::VecModel::from(indexed));
    ui.set_channel_items(model.into());
}

pub fn reflow_grid(ui: &MainWindow, catalog_columns: usize, channel_columns: usize) {
    let catalog = ui.get_catalog_items();
    let catalog_cards = (0..catalog.row_count())
        .filter_map(|index| catalog.row_data(index))
        .collect::<Vec<_>>();
    let catalog_rows = Rc::new(slint::VecModel::from(cards_to_rows(
        &catalog_cards,
        catalog_columns,
    )));
    ui.set_catalog_rows(catalog_rows.into());

    let channels = ui.get_channel_items();
    let channel_cards = (0..channels.row_count())
        .filter_map(|index| channels.row_data(index))
        .collect::<Vec<_>>();
    let channel_rows = Rc::new(slint::VecModel::from(cards_to_rows(
        &channel_cards,
        channel_columns,
    )));
    ui.set_channel_rows(channel_rows.into());
}

fn update_catalog_thumbnail(
    ui: &MainWindow,
    index: usize,
    request: &ThumbnailRequest,
    thumbnail: &FetchedThumbnail,
) {
    let items = ui.get_catalog_items();
    let Some(mut card) = items.row_data(index) else {
        return;
    };
    if !thumbnail_request_matches_card(request, &card) {
        return;
    }
    let Some(image) = crate::thumbnails::image_from_bytes(&thumbnail.bytes) else {
        update_thumbnail_state(ui, index, &request.id, "failed", false);
        return;
    };
    card.thumbnail = image;
    card.thumbnail_state = "ready".into();
    card.has_thumbnail = true;
    items.set_row_data(index, card.clone());

    let rows = ui.get_catalog_rows();
    let columns = ui.get_catalog_column_count().max(1) as usize;
    let row_index = index / columns;
    if let Some(mut row) = rows.row_data(row_index) {
        match index % columns {
            0 => row.card_0 = card,
            1 => row.card_1 = card,
            2 => row.card_2 = card,
            _ => row.card_3 = card,
        }
        rows.set_row_data(row_index, row);
    }
}

fn update_channel_thumbnail(
    ui: &MainWindow,
    index: usize,
    request: &ThumbnailRequest,
    thumbnail: &FetchedThumbnail,
) {
    let items = ui.get_channel_items();
    let Some(mut card) = items.row_data(index) else {
        return;
    };
    if !thumbnail_request_matches_card(request, &card) {
        return;
    }
    let Some(image) = crate::thumbnails::image_from_bytes(&thumbnail.bytes) else {
        update_thumbnail_state(ui, index, &request.id, "failed", true);
        return;
    };
    card.thumbnail = image;
    card.thumbnail_state = "ready".into();
    card.has_thumbnail = true;
    items.set_row_data(index, card.clone());

    let rows = ui.get_channel_rows();
    let columns = ui.get_channel_column_count().max(1) as usize;
    let row_index = index / columns;
    if let Some(mut row) = rows.row_data(row_index) {
        match index % columns {
            0 => row.card_0 = card,
            1 => row.card_1 = card,
            2 => row.card_2 = card,
            _ => row.card_3 = card,
        }
        rows.set_row_data(row_index, row);
    }
}

fn update_thumbnail_state(
    ui: &MainWindow,
    index: usize,
    expected_id: &str,
    state: &str,
    channel: bool,
) {
    let (items, rows, columns) = if channel {
        (
            ui.get_channel_items(),
            ui.get_channel_rows(),
            ui.get_channel_column_count().max(1) as usize,
        )
    } else {
        (
            ui.get_catalog_items(),
            ui.get_catalog_rows(),
            ui.get_catalog_column_count().max(1) as usize,
        )
    };
    let Some(mut card) = items.row_data(index) else {
        return;
    };
    if card.id != expected_id {
        return;
    }
    card.thumbnail_state = state.into();
    items.set_row_data(index, card.clone());

    let row_index = index / columns;
    if let Some(mut row) = rows.row_data(row_index) {
        match index % columns {
            0 => row.card_0 = card,
            1 => row.card_1 = card,
            2 => row.card_2 = card,
            _ => row.card_3 = card,
        }
        rows.set_row_data(row_index, row);
    }
}

pub fn retry_thumbnail_async(
    ui: slint::Weak<MainWindow>,
    request: ThumbnailRequest,
    token: Option<String>,
    channel: bool,
) {
    let Some(handle) = ui.upgrade() else { return };
    let items = if channel {
        handle.get_channel_items()
    } else {
        handle.get_catalog_items()
    };
    let Some(index) = card_index_by_request(&items, &request) else {
        return;
    };
    if request.primary_url.is_empty()
        && request.fallback_url.is_empty()
        && request.alternate_url.is_empty()
    {
        update_thumbnail_state(&handle, index, &request.id, "missing", channel);
        return;
    }

    update_thumbnail_state(&handle, index, &request.id, "loading", channel);
    let weak = ui.clone();
    std::thread::spawn(move || {
        invalidate_thumbnail_request_cache(&request);
        let thumbnail = fetch_card_thumbnail(&request, token.as_deref());
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            let items = if channel {
                ui.get_channel_items()
            } else {
                ui.get_catalog_items()
            };
            let Some(index) = card_index_by_request(&items, &request) else {
                return;
            };
            if let Some(thumbnail) = thumbnail {
                if channel {
                    update_channel_thumbnail(&ui, index, &request, &thumbnail);
                } else {
                    update_catalog_thumbnail(&ui, index, &request, &thumbnail);
                }
            } else {
                update_thumbnail_state(&ui, index, &request.id, "failed", channel);
            }
        });
    });
}

fn home_thumbnail_models(ui: &MainWindow) -> Vec<slint::ModelRc<VideoCardData>> {
    let mut models = vec![
        ui.get_home_resume_items(),
        ui.get_home_history_items(),
        ui.get_home_latest_items(),
        ui.get_home_random_items(),
        ui.get_home_performer_items(),
    ];
    let shelves = ui.get_home_category_shelves();
    for index in 0..shelves.row_count() {
        if let Some(shelf) = shelves.row_data(index) {
            models.push(shelf.items);
        }
    }
    models
}

fn update_home_thumbnail_state(ui: &MainWindow, request: &ThumbnailRequest, state: &str) {
    for model in home_thumbnail_models(ui) {
        for index in 0..model.row_count() {
            let Some(mut card) = model.row_data(index) else {
                continue;
            };
            if !thumbnail_request_matches_card(&request, &card) {
                continue;
            }
            card.thumbnail_state = state.into();
            model.set_row_data(index, card);
        }
    }
}

pub fn retry_home_thumbnail_async(
    ui: slint::Weak<MainWindow>,
    request: ThumbnailRequest,
    token: Option<String>,
) {
    let Some(handle) = ui.upgrade() else { return };
    update_home_thumbnail_state(&handle, &request, "loading");
    if request.primary_url.is_empty()
        && request.fallback_url.is_empty()
        && request.alternate_url.is_empty()
    {
        update_home_thumbnail_state(&handle, &request, "missing");
        return;
    }
    std::thread::spawn(move || {
        invalidate_thumbnail_request_cache(&request);
        let thumbnail = fetch_card_thumbnail(&request, token.as_deref());
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui.upgrade() else { return };
            for model in home_thumbnail_models(&ui) {
                for index in 0..model.row_count() {
                    let Some(mut card) = model.row_data(index) else {
                        continue;
                    };
                    if !thumbnail_request_matches_card(&request, &card) {
                        continue;
                    }
                    if let Some(thumbnail) = thumbnail.as_ref() {
                        if let Some(image) = crate::thumbnails::image_from_bytes(&thumbnail.bytes) {
                            card.thumbnail = image;
                            card.thumbnail_state = "ready".into();
                            card.has_thumbnail = true;
                        } else {
                            card.thumbnail_state = "failed".into();
                        }
                    } else {
                        card.thumbnail_state = "failed".into();
                    }
                    model.set_row_data(index, card);
                }
            }
        });
    });
}

pub fn set_discovered_servers(ui: &MainWindow, items: Vec<DiscoveredServerItem>) {
    let model = Rc::new(slint::VecModel::from(items));
    ui.set_discovered_servers(model.into());
}

fn titlecase(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = true;
    for c in s.chars() {
        if c.is_whitespace() {
            capitalize_next = true;
            result.push(c);
        } else if capitalize_next {
            result.extend(c.to_uppercase());
            capitalize_next = false;
        } else {
            result.extend(c.to_lowercase());
        }
    }
    result
}

pub(crate) fn display_title(item: &TvItem) -> String {
    let title = item.title.trim();
    let is_hash = |s: &str| -> bool {
        let clean = s.trim();
        (clean.len() >= 24 && clean.chars().all(|c| c.is_ascii_hexdigit()))
            || (clean.len() == 36 && clean.chars().filter(|&c| c == '-').count() == 4)
    };

    // 1. Poprawny tytuł
    if !title.is_empty()
        && title != item.id
        && !is_hash(title)
        && !title.starts_with("telegram:")
        && !title.starts_with("direct:")
        && !title.eq_ignore_ascii_case("untitled")
        && !title.eq_ignore_ascii_case("untitled video")
    {
        if let Some(pos) = title.rfind('.') {
            let ext = &title[pos + 1..];
            if matches!(
                ext.to_ascii_lowercase().as_str(),
                "mp4" | "mkv" | "avi" | "mov" | "webm" | "ts" | "m4v" | "wmv"
            ) {
                let stem = &title[..pos];
                if !stem.is_empty() && !is_hash(stem) {
                    return stem.replace(['_', '.'], " ").trim().to_string();
                }
            }
        }
        return title.to_string();
    }

    // 2. Czytelna nazwa pliku z item.id lub ścieżki
    let source_id = item.id.strip_prefix("direct:").unwrap_or(&item.id);
    if let Some(pos) = source_id.rfind('.') {
        let ext = &source_id[pos + 1..];
        if matches!(
            ext.to_ascii_lowercase().as_str(),
            "mp4" | "mkv" | "avi" | "mov" | "webm" | "ts" | "m4v" | "wmv"
        ) {
            let filename = source_id[..pos]
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&source_id[..pos]);
            if !filename.is_empty() && !is_hash(filename) {
                return filename.replace(['_', '.'], " ").trim().to_string();
            }
        }
    }

    // 3. Źródło lub kanał wraz z dostępnym kontekstem
    if item.is_telegram() || item.id.starts_with("telegram:") {
        let parts: Vec<&str> = item.id.split(':').collect();
        if parts.len() >= 3 {
            let channel_raw = parts[1].replace(['_', '-'], " ");
            let channel = titlecase(&channel_raw);
            let msg_id = parts[2];
            if !channel.is_empty() {
                if let Some(studio) = item.studios.first().filter(|s| !s.trim().is_empty()) {
                    return format!("{studio} #{msg_id}");
                }
                return format!("{channel} #{msg_id}");
            }
        }
    }
    if let Some(studio) = item.studios.first().filter(|s| !s.trim().is_empty()) {
        if let Some(year) = item.year {
            return format!("{studio} ({year})");
        }
        return format!("{studio} Video");
    }

    // 4. Neutralna nazwa z krótkim rozróżniającym oznaczeniem
    let short_marker = if let Some(pos) = item.id.rfind(':') {
        &item.id[pos + 1..]
    } else if item.id.len() >= 6 {
        &item.id[..6]
    } else {
        &item.id
    };
    if !short_marker.is_empty() && short_marker != "sample" {
        format!("Video #{short_marker}")
    } else {
        "Video".to_string()
    }
}

pub fn tv_item_to_card(item: &TvItem) -> VideoCardData {
    let year_str = item.year.map(|y| y.to_string()).unwrap_or_default();
    let duration_str = item.formatted_duration();
    let provider = if item.is_telegram() {
        "telegram".to_string()
    } else if let Some(provider) = item.source_provider.as_deref() {
        provider.to_ascii_lowercase()
    } else if item.genres.iter().any(|tag| tag == "__adult_hub_performer") {
        "performer".to_string()
    } else if item.genres.iter().any(|tag| tag == "__adult_hub_studio") {
        "studio".to_string()
    } else if item.genres.iter().any(|tag| tag == "__adult_hub_genre") {
        "genre".to_string()
    } else if item.id == "sample" || item.id.starts_with('/') || item.id.contains(":\\") {
        "local".to_string()
    } else {
        "jellyfin".to_string()
    };
    let progress_val = if item.is_played {
        1.0
    } else if let Some(pct) = item.played_percentage {
        (pct as f32) / 100.0
    } else if item.playback_position_ticks > 0 && item.runtime_ticks.unwrap_or(0) > 0 {
        (item.playback_position_ticks as f32) / (item.runtime_ticks.unwrap() as f32)
    } else {
        0.0
    };
    let is_played = item.is_played || progress_val >= 0.90;

    let thumbnail_url = if item.source_provider.is_some() {
        item.primary_image_tag
            .as_deref()
            .and_then(crate::thumbnails::resolve_resolver_thumbnail_url)
            .unwrap_or_default()
    } else {
        String::new()
    };
    let fallback_thumbnail_url = if item.is_telegram() {
        crate::thumbnails::phash_thumbnail_url(&item.id).unwrap_or_default()
    } else {
        String::new()
    };
    let cache_key = if !thumbnail_url.is_empty() {
        thumbnail_url.as_str()
    } else if !fallback_thumbnail_url.is_empty() {
        fallback_thumbnail_url.as_str()
    } else {
        item.id.as_str()
    };
    let (has_thumbnail, thumbnail) = if !cache_key.is_empty() {
        if let Some(img) = crate::thumbnails::load_cached_image(cache_key) {
            (true, img)
        } else {
            (false, slint::Image::default())
        }
    } else {
        (false, slint::Image::default())
    };

    VideoCardData {
        id: item.id.clone().into(),
        title: display_title(item).into(),
        genre: item
            .genres
            .iter()
            .find(|genre| !genre.starts_with("__adult_hub_"))
            .map(|genre| genre.to_uppercase())
            .unwrap_or_default()
            .into(),
        duration: duration_str.into(),
        year: year_str.into(),
        is_played,
        provider: provider.into(),
        is_favorite: item.is_favorite,
        studio: if item.genres.iter().any(|tag| tag == "__adult_hub_performer") {
            item.child_count
                .filter(|count| *count > 0)
                .map(|count| format!("{count} movies"))
                .unwrap_or_default()
        } else {
            item.studios.first().cloned().unwrap_or_default()
        }
        .into(),
        progress: progress_val.clamp(0.0, 1.0),
        has_thumbnail,
        thumbnail_state: if has_thumbnail {
            "ready"
        } else if !thumbnail_url.is_empty() || !fallback_thumbnail_url.is_empty() {
            "loading"
        } else {
            "missing"
        }
        .into(),
        thumbnail,
        card_index: 0,
        thumbnail_url: thumbnail_url.into(),
        fallback_thumbnail_url: fallback_thumbnail_url.into(),
        alternate_thumbnail_url: "".into(),
    }
}

pub fn tv_item_to_card_for_server(item: &TvItem, server_url: &str) -> VideoCardData {
    let mut card = tv_item_to_card(item);
    let (image_url, fallback_image_url, alternate_image_url) = if item.source_provider.is_some() {
        (
            item.primary_image_tag
                .as_deref()
                .and_then(crate::thumbnails::resolve_resolver_thumbnail_url),
            None,
            None,
        )
    } else {
        let image_width = crate::thumbnails::thumbnail_max_width();
        // Match Android's Jellyfin choice: use Thumb when present, then Backdrop,
        // then Primary. Keep the other image types as desktop fallbacks.
        let preferred = if item
            .thumb_image_tag
            .as_deref()
            .is_some_and(|tag| !tag.trim().is_empty())
        {
            "Thumb"
        } else if item
            .backdrop_image_tag
            .as_deref()
            .is_some_and(|tag| !tag.trim().is_empty())
        {
            "Backdrop"
        } else {
            "Primary"
        };
        let order = match preferred {
            "Thumb" => ["Thumb", "Primary", "Backdrop"],
            "Backdrop" => ["Backdrop", "Primary", "Thumb"],
            _ => ["Primary", "Thumb", "Backdrop"],
        };
        let mut candidates = order.into_iter().map(|image_type| {
            let tag = match image_type {
                "Thumb" => item.thumb_image_tag.as_deref(),
                "Backdrop" => item.backdrop_image_tag.as_deref(),
                _ => item.primary_image_tag.as_deref(),
            }
            .filter(|tag| !tag.trim().is_empty());
            JellyfinClient::image_url(server_url, &item.id, image_type, image_width, tag)
        });
        let preferred = candidates.next().expect("three image candidates");
        let fallback = candidates.next().expect("three image candidates");
        let alternate = candidates.next().expect("three image candidates");
        (Some(preferred), Some(fallback), Some(alternate))
    };

    if let Some(url) = image_url {
        card.thumbnail_url = url.clone().into();
        if item.source_provider.is_none() {
            card.fallback_thumbnail_url = fallback_image_url.unwrap_or_default().into();
            card.alternate_thumbnail_url = alternate_image_url.unwrap_or_default().into();
        }
        if let Some(image) = crate::thumbnails::load_cached_image(&url) {
            card.has_thumbnail = true;
            card.thumbnail_state = "ready".into();
            card.thumbnail = image;
        } else {
            card.thumbnail_state = "loading".into();
        }
    }
    card
}

fn ensure_thumbnail_variant(url: &str, token: Option<&str>) -> Option<(String, Vec<u8>)> {
    let ensure = |candidate: &str| {
        crate::thumbnails::load_cached_image_bytes(candidate)
            .map(|bytes| (candidate.to_owned(), bytes))
            .or_else(|| {
                crate::thumbnails::fetch_and_cache_image_bytes(candidate, token)
                    .map(|bytes| (candidate.to_owned(), bytes))
            })
    };
    ensure(url).or_else(|| {
        let mut untagged = url::Url::parse(url).ok()?;
        let params = untagged
            .query_pairs()
            .filter(|(key, _)| key != "tag")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        if params.len() == untagged.query_pairs().count() {
            return None;
        }
        untagged.set_query(None);
        if !params.is_empty() {
            untagged.query_pairs_mut().extend_pairs(params);
        }
        ensure(untagged.as_str())
    })
}

fn indexed_backdrop_url(url: &str) -> Option<String> {
    let mut parsed = url::Url::parse(url).ok()?;
    let segments = parsed
        .path_segments()?
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let images_index = segments
        .iter()
        .rposition(|segment| segment.eq_ignore_ascii_case("Images"))?;
    if segments
        .get(images_index + 1)?
        .eq_ignore_ascii_case("Backdrop")
        && images_index + 2 == segments.len()
    {
        parsed.path_segments_mut().ok()?.push("0");
        Some(parsed.to_string())
    } else {
        None
    }
}

fn ensure_thumbnail(url: &str, token: Option<&str>) -> Option<(String, Vec<u8>)> {
    if url.is_empty() {
        return None;
    }
    ensure_thumbnail_variant(url, token).or_else(|| {
        let indexed = indexed_backdrop_url(url)?;
        ensure_thumbnail_variant(&indexed, token)
    })
}

/// Fetches and caches artwork on workers, returning only plain URLs. Slint
/// images are created later on the UI thread, so its non-thread-safe image
/// handles never cross the event loop boundary.
pub fn fetch_card_thumbnail(
    request: &ThumbnailRequest,
    token: Option<&str>,
) -> Option<FetchedThumbnail> {
    let key = thumbnail_flight_key(request, token);
    let flights = THUMBNAIL_FLIGHTS.get_or_init(|| Mutex::new(HashMap::new()));
    let (flight, is_leader) = {
        let mut flights = flights.lock().ok()?;
        if let Some(flight) = flights.get(&key) {
            (flight.clone(), false)
        } else {
            let flight = Arc::new(ThumbnailFlight {
                result: Mutex::new(None),
                completed: Condvar::new(),
            });
            flights.insert(key.clone(), flight.clone());
            (flight, true)
        }
    };

    if !is_leader {
        let mut result = flight.result.lock().ok()?;
        while result.is_none() {
            result = flight.completed.wait(result).ok()?;
        }
        return result.clone().flatten();
    }

    // The permit caps concurrent network/decode work across all screens.
    // Catch panics at this worker boundary so every in-flight request still
    // resolves to a terminal Failed result and wakes deduplicated waiters.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _permit = ThumbnailDownloadPermit::acquire()?;
        fetch_card_thumbnail_inner(request, token)
    }))
    .unwrap_or(None);

    if let Ok(mut stored) = flight.result.lock() {
        *stored = Some(result.clone());
        flight.completed.notify_all();
    }
    if let Ok(mut flights) = flights.lock() {
        flights.remove(&key);
    }
    result
}

fn fetch_card_thumbnail_inner(
    request: &ThumbnailRequest,
    token: Option<&str>,
) -> Option<FetchedThumbnail> {
    let fallback_first = request.provider == "telegram" && request.progress > 0.0;
    let candidates = if fallback_first {
        [
            request.fallback_url.as_str(),
            request.primary_url.as_str(),
            request.alternate_url.as_str(),
        ]
    } else {
        [
            request.primary_url.as_str(),
            request.fallback_url.as_str(),
            request.alternate_url.as_str(),
        ]
    };

    let mut last_resort = None;
    for candidate in candidates {
        let Some((url, bytes)) = ensure_thumbnail(candidate, token) else {
            continue;
        };
        let fetched = FetchedThumbnail { url, bytes };
        if !crate::thumbnails::is_poor_thumbnail_bytes(&fetched.bytes) {
            return Some(fetched);
        }
        // Keep a decoded image as a last resort, but let later Jellyfin image
        // types replace blank/near-black placeholders when they are available.
        last_resort.get_or_insert(fetched);
    }
    last_resort
}

pub fn fetch_thumbnail_batch(
    requests: &[ThumbnailRequest],
    token: Option<&str>,
) -> Vec<(usize, FetchedThumbnail)> {
    std::thread::scope(|scope| {
        let jobs = requests
            .iter()
            .enumerate()
            .map(|(index, request)| {
                scope.spawn(move || fetch_card_thumbnail(request, token).map(|url| (index, url)))
            })
            .collect::<Vec<_>>();
        jobs.into_iter()
            .filter_map(|job| job.join().ok().flatten())
            .collect()
    })
}

pub fn refresh_catalog_thumbnails_async(
    ui: slint::Weak<MainWindow>,
    cards: Vec<VideoCardData>,
    token: String,
    _current_items: Arc<Mutex<Vec<TvItem>>>,
    generation: Option<(Arc<AtomicU64>, u64)>,
) {
    let requests = cards.iter().map(ThumbnailRequest::from).collect::<Vec<_>>();
    std::thread::spawn(move || {
        for start in (0..requests.len()).step_by(THUMBNAIL_BATCH_SIZE) {
            if generation.as_ref().is_some_and(|(generation, request_id)| {
                !thumbnail_generation_is_current(generation, *request_id)
            }) {
                break;
            }
            let end = (start + THUMBNAIL_BATCH_SIZE).min(requests.len());
            let thumbnails = fetch_thumbnail_batch(&requests[start..end], Some(&token));
            let successful_offsets = thumbnails
                .iter()
                .map(|(offset, _)| *offset)
                .collect::<Vec<_>>();
            let source_available = requests[start..end]
                .iter()
                .map(|request| {
                    !request.primary_url.is_empty()
                        || !request.fallback_url.is_empty()
                        || !request.alternate_url.is_empty()
                })
                .collect::<Vec<_>>();
            let batch_requests = requests[start..end].to_vec();
            let generation = generation.clone();
            let ui = ui.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if generation.as_ref().is_some_and(|(generation, request_id)| {
                    !thumbnail_generation_is_current(generation, *request_id)
                }) {
                    return;
                }
                if let Some(ui) = ui.upgrade() {
                    for (offset, thumbnail) in thumbnails {
                        if let Some(request) = batch_requests.get(offset) {
                            let items = ui.get_catalog_items();
                            if let Some(index) = card_index_by_request(&items, request) {
                                update_catalog_thumbnail(&ui, index, request, &thumbnail);
                            }
                        }
                    }
                    for offset in 0..(end - start) {
                        if successful_offsets.contains(&offset) {
                            continue;
                        }
                        if let Some(request) = batch_requests.get(offset) {
                            let state = no_thumbnail_state(
                                source_available.get(offset).copied().unwrap_or(false),
                            );
                            let items = ui.get_catalog_items();
                            if let Some(index) = card_index_by_request(&items, request) {
                                update_thumbnail_state(&ui, index, &request.id, state, false);
                            }
                        }
                    }
                }
            });
        }
    });
}

pub fn refresh_channel_thumbnails_async(
    ui: slint::Weak<MainWindow>,
    cards: Vec<VideoCardData>,
    _current_items: Arc<Mutex<Vec<TvItem>>>,
    generation: Arc<AtomicU64>,
    request_id: u64,
) {
    let requests = cards.iter().map(ThumbnailRequest::from).collect::<Vec<_>>();
    std::thread::spawn(move || {
        for start in (0..requests.len()).step_by(THUMBNAIL_BATCH_SIZE) {
            if !thumbnail_generation_is_current(&generation, request_id) {
                break;
            }
            let end = (start + THUMBNAIL_BATCH_SIZE).min(requests.len());
            let thumbnails = fetch_thumbnail_batch(&requests[start..end], None);
            let successful_offsets = thumbnails
                .iter()
                .map(|(offset, _)| *offset)
                .collect::<Vec<_>>();
            let source_available = requests[start..end]
                .iter()
                .map(|request| {
                    !request.primary_url.is_empty()
                        || !request.fallback_url.is_empty()
                        || !request.alternate_url.is_empty()
                })
                .collect::<Vec<_>>();
            let batch_requests = requests[start..end].to_vec();
            let generation = generation.clone();
            let ui = ui.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if !thumbnail_generation_is_current(&generation, request_id) {
                    return;
                }
                if let Some(ui) = ui.upgrade() {
                    for (offset, thumbnail) in thumbnails {
                        if let Some(request) = batch_requests.get(offset) {
                            let items = ui.get_channel_items();
                            if let Some(index) = card_index_by_request(&items, request) {
                                update_channel_thumbnail(&ui, index, request, &thumbnail);
                            }
                        }
                    }
                    for offset in 0..(end - start) {
                        if successful_offsets.contains(&offset) {
                            continue;
                        }
                        if let Some(request) = batch_requests.get(offset) {
                            let state = no_thumbnail_state(
                                source_available.get(offset).copied().unwrap_or(false),
                            );
                            let items = ui.get_channel_items();
                            if let Some(index) = card_index_by_request(&items, request) {
                                update_thumbnail_state(&ui, index, &request.id, state, true);
                            }
                        }
                    }
                }
            });
        }
    });
}

pub fn filter_catalog_items(items: Vec<TvItem>, local_store: &LocalStore) -> Vec<TvItem> {
    let excluded_tags = local_store.get_excluded_tags().unwrap_or_default();
    let hidden_channels = local_store.get_hidden_channels().unwrap_or_default();

    if excluded_tags.is_empty() && hidden_channels.is_empty() {
        return items;
    }

    items
        .into_iter()
        .filter(|item| {
            let matches_excluded_tag = excluded_tags.iter().any(|excluded| {
                item.genres
                    .iter()
                    .chain(item.tags.iter())
                    .any(|value| value.eq_ignore_ascii_case(excluded))
                    || item.title.to_lowercase().contains(&excluded.to_lowercase())
                    || item
                        .people
                        .iter()
                        .any(|person| person.name.eq_ignore_ascii_case(excluded))
            });
            if matches_excluded_tag {
                return false;
            }
            if let Some(prov_id) = &item.provider_video_id {
                if let Some(ch) = prov_id.split(':').next() {
                    if hidden_channels.iter().any(|h| h.eq_ignore_ascii_case(ch)) {
                        return false;
                    }
                }
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, title: &str) -> VideoCardData {
        VideoCardData {
            id: id.into(),
            title: title.into(),
            genre: String::new().into(),
            duration: String::new().into(),
            year: String::new().into(),
            is_played: false,
            provider: "telegram".into(),
            is_favorite: false,
            studio: String::new().into(),
            progress: 0.0,
            has_thumbnail: false,
            thumbnail_state: "not-requested".into(),
            thumbnail: slint::Image::default(),
            card_index: 0,
            thumbnail_url: String::new().into(),
            fallback_thumbnail_url: String::new().into(),
            alternate_thumbnail_url: String::new().into(),
        }
    }

    #[test]
    fn hashes_and_identifiers_use_a_neutral_card_title() {
        let item = TvItem {
            id: "telegram:channel:123".into(),
            title: "8b61a0c8267a2e346d96e68b07d94354".into(),
            ..Default::default()
        };
        assert_eq!(display_title(&item), "Channel #123");
        let hash_only = TvItem {
            id: "8b61a0c8267a2e346d96e68b07d94354".into(),
            title: "8b61a0c8267a2e346d96e68b07d94354".into(),
            ..Default::default()
        };
        assert_eq!(display_title(&hash_only), "Video #8b61a0");
        let filename_item = TvItem {
            id: "sample.mp4".into(),
            title: "my_summer_video.mp4".into(),
            ..Default::default()
        };
        assert_eq!(display_title(&filename_item), "my summer video");
        let named = TvItem {
            title: "A named film".into(),
            ..item
        };
        assert_eq!(display_title(&named), "A named film");
    }

    #[test]
    fn failed_and_missing_thumbnail_states_are_distinct() {
        assert_eq!(no_thumbnail_state(true), "failed");
        assert_eq!(no_thumbnail_state(false), "missing");
    }

    #[test]
    fn timeout_or_cancelled_request_with_a_known_source_is_not_missing() {
        assert_eq!(no_thumbnail_state(true), "failed");
        assert_eq!(no_thumbnail_state(false), "missing");
    }

    #[test]
    fn cancelled_generation_is_not_current_for_later_batches() {
        let generation = AtomicU64::new(7);
        assert!(thumbnail_generation_is_current(&generation, 7));
        generation.store(8, Ordering::SeqCst);
        assert!(!thumbnail_generation_is_current(&generation, 7));
    }

    #[test]
    fn stale_thumbnail_results_are_matched_by_id_after_reordering() {
        let model: slint::ModelRc<VideoCardData> = Rc::new(slint::VecModel::from(vec![
            card("item-b", "B"),
            card("item-a", "A"),
        ]))
        .into();
        let request = ThumbnailRequest::from(&card("item-a", "A"));
        assert_eq!(card_index_by_request(&model, &request), Some(1));
        let missing_request = ThumbnailRequest::from(&card("removed-item", "Missing"));
        assert_eq!(card_index_by_request(&model, &missing_request), None);
    }

    #[test]
    fn stale_thumbnail_result_is_rejected_when_same_id_now_has_another_source() {
        let mut original = card("same-id", "Original");
        original.thumbnail_url = "https://resolver.example/image/one".into();
        let request = ThumbnailRequest::from(&original);

        let mut replacement = card("same-id", "Replacement");
        replacement.thumbnail_url = "https://resolver.example/image/two".into();
        let model: slint::ModelRc<VideoCardData> =
            Rc::new(slint::VecModel::from(vec![replacement])).into();

        assert_eq!(card_index_by_request(&model, &request), None);
    }

    #[test]
    fn request_deduplication_key_is_stable_but_separated_by_credentials() {
        let mut request = ThumbnailRequest::from(&card("telegram:chan:1", "Video"));
        request.primary_url = "https://resolver.example/admin/thumb?id=anonymous".into();
        let first = thumbnail_flight_key(&request, Some("token-one"));
        assert_eq!(first, thumbnail_flight_key(&request, Some("token-one")));
        assert_ne!(first, thumbnail_flight_key(&request, Some("token-two")));
        assert_ne!(first, thumbnail_flight_key(&request, None));
    }

    #[test]
    fn thumbnail_downloads_are_globally_bounded() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        let active = AtomicUsize::new(0);
        let highest = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..12 {
                let active = &active;
                let highest = &highest;
                scope.spawn(move || {
                    let _permit = ThumbnailDownloadPermit::acquire().unwrap();
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    highest.fetch_max(current, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(10));
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        assert!(highest.load(Ordering::SeqCst) <= 4);
    }
}
