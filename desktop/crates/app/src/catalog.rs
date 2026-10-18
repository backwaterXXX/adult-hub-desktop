use crate::{DiscoveredServerItem, MainWindow, VideoCardData, VideoCardRowData};
use adult_hub_client::{JellyfinClient, TvItem, TvPersonRole};
use adult_hub_storage::LocalStore;
use slint::Model;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct ThumbnailRequest {
    pub id: String,
    pub primary_url: String,
    pub fallback_url: String,
    pub alternate_url: String,
    pub provider: String,
    pub progress: f32,
}

pub struct FetchedThumbnail {
    pub url: String,
    pub bytes: Vec<u8>,
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

fn card_index_by_id(model: &slint::ModelRc<VideoCardData>, id: &str) -> Option<usize> {
    (0..model.row_count()).find(|index| model.row_data(*index).is_some_and(|card| card.id == id))
}

fn no_thumbnail_state(has_source_url: bool) -> &'static str {
    if has_source_url { "failed" } else { "missing" }
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
    expected_id: &str,
    thumbnail: &FetchedThumbnail,
) {
    let items = ui.get_catalog_items();
    let Some(mut card) = items.row_data(index) else {
        return;
    };
    if card.id != expected_id {
        return;
    }
    let Some(image) = crate::thumbnails::image_from_bytes(&thumbnail.bytes) else {
        update_thumbnail_state(ui, index, expected_id, "failed", false);
        return;
    };
    card.thumbnail = image;
    card.thumbnail_state = "ready".into();
    card.thumbnail_url = thumbnail.url.clone().into();
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

fn update_channel_thumbnail(ui: &MainWindow, index: usize, thumbnail: &FetchedThumbnail) {
    let items = ui.get_channel_items();
    let Some(mut card) = items.row_data(index) else {
        return;
    };
    let Some(image) = crate::thumbnails::image_from_bytes(&thumbnail.bytes) else {
        if let Some(id) = items.row_data(index).map(|card| card.id.to_string()) {
            update_thumbnail_state(ui, index, &id, "failed", true);
        }
        return;
    };
    card.thumbnail = image;
    card.thumbnail_state = "ready".into();
    card.thumbnail_url = thumbnail.url.clone().into();
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
    id: String,
    token: Option<String>,
    channel: bool,
) {
    let Some(handle) = ui.upgrade() else { return };
    let items = if channel {
        handle.get_channel_items()
    } else {
        handle.get_catalog_items()
    };
    let Some((index, card)) = (0..items.row_count())
        .filter_map(|index| items.row_data(index).map(|card| (index, card)))
        .find(|(_, card)| card.id == id)
    else {
        return;
    };
    if card.thumbnail_url.is_empty()
        && card.fallback_thumbnail_url.is_empty()
        && card.alternate_thumbnail_url.is_empty()
    {
        update_thumbnail_state(&handle, index, &id, "missing", channel);
        return;
    }

    update_thumbnail_state(&handle, index, &id, "loading", channel);
    let request = ThumbnailRequest::from(&card);
    let weak = ui.clone();
    std::thread::spawn(move || {
        let thumbnail = fetch_card_thumbnail(&request, token.as_deref());
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            let items = if channel {
                ui.get_channel_items()
            } else {
                ui.get_catalog_items()
            };
            let Some(index) = (0..items.row_count())
                .find(|index| items.row_data(*index).is_some_and(|card| card.id == id))
            else {
                return;
            };
            if let Some(thumbnail) = thumbnail {
                if channel {
                    update_channel_thumbnail(&ui, index, &thumbnail);
                } else {
                    update_catalog_thumbnail(&ui, index, &id, &thumbnail);
                }
            } else {
                update_thumbnail_state(&ui, index, &id, "failed", channel);
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

fn update_home_thumbnail_state(ui: &MainWindow, id: &str, state: &str) -> Option<ThumbnailRequest> {
    let mut request = None;
    for model in home_thumbnail_models(ui) {
        for index in 0..model.row_count() {
            let Some(mut card) = model.row_data(index) else {
                continue;
            };
            if card.id != id {
                continue;
            }
            card.thumbnail_state = state.into();
            request.get_or_insert_with(|| ThumbnailRequest::from(&card));
            model.set_row_data(index, card);
        }
    }
    request
}

pub fn retry_home_thumbnail_async(ui: slint::Weak<MainWindow>, id: String, token: Option<String>) {
    let Some(handle) = ui.upgrade() else { return };
    let Some(request) = update_home_thumbnail_state(&handle, &id, "loading") else {
        return;
    };
    if request.primary_url.is_empty()
        && request.fallback_url.is_empty()
        && request.alternate_url.is_empty()
    {
        update_home_thumbnail_state(&handle, &id, "missing");
        return;
    }
    std::thread::spawn(move || {
        let thumbnail = fetch_card_thumbnail(&request, token.as_deref());
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui.upgrade() else { return };
            for model in home_thumbnail_models(&ui) {
                for index in 0..model.row_count() {
                    let Some(mut card) = model.row_data(index) else {
                        continue;
                    };
                    if card.id != id {
                        continue;
                    }
                    if let Some(thumbnail) = thumbnail.as_ref() {
                        if let Some(image) = crate::thumbnails::image_from_bytes(&thumbnail.bytes) {
                            card.thumbnail = image;
                            card.thumbnail_state = "ready".into();
                            card.thumbnail_url = thumbnail.url.clone().into();
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

fn display_title(item: &TvItem) -> String {
    let title = item.title.trim();
    let looks_like_hash =
        title.len() >= 24 && title.chars().all(|character| character.is_ascii_hexdigit());
    if title.is_empty() || title == item.id || looks_like_hash || title.starts_with("telegram:") {
        "Untitled video".into()
    } else {
        title.into()
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
    let expected_ids = requests
        .iter()
        .map(|card| card.id.clone())
        .collect::<Vec<_>>();
    std::thread::spawn(move || {
        for start in (0..requests.len()).step_by(6) {
            let end = (start + 6).min(requests.len());
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
            let expected_ids = expected_ids.clone();
            let generation = generation.clone();
            let ui = ui.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if generation.as_ref().is_some_and(|(generation, request_id)| {
                    generation.load(Ordering::SeqCst) != *request_id
                }) {
                    return;
                }
                if let Some(ui) = ui.upgrade() {
                    for (offset, thumbnail) in thumbnails {
                        if let Some(expected_id) = expected_ids.get(start + offset) {
                            let items = ui.get_catalog_items();
                            if let Some(index) = card_index_by_id(&items, expected_id) {
                                update_catalog_thumbnail(&ui, index, expected_id, &thumbnail);
                            }
                        }
                    }
                    for offset in 0..(end - start) {
                        if successful_offsets.contains(&offset) {
                            continue;
                        }
                        if let Some(expected_id) = expected_ids.get(start + offset) {
                            let state = no_thumbnail_state(
                                source_available.get(offset).copied().unwrap_or(false),
                            );
                            let items = ui.get_catalog_items();
                            if let Some(index) = card_index_by_id(&items, expected_id) {
                                update_thumbnail_state(&ui, index, expected_id, state, false);
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
    let expected_ids = requests
        .iter()
        .map(|card| card.id.clone())
        .collect::<Vec<_>>();
    std::thread::spawn(move || {
        for start in (0..requests.len()).step_by(6) {
            let end = (start + 6).min(requests.len());
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
            let expected_ids = expected_ids.clone();
            let generation = generation.clone();
            let ui = ui.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if generation.load(Ordering::SeqCst) != request_id {
                    return;
                }
                if let Some(ui) = ui.upgrade() {
                    for (offset, thumbnail) in thumbnails {
                        if let Some(expected_id) = expected_ids.get(start + offset) {
                            let items = ui.get_channel_items();
                            if let Some(index) = card_index_by_id(&items, expected_id) {
                                if items
                                    .row_data(index)
                                    .is_some_and(|card| card.id == *expected_id)
                                {
                                    update_channel_thumbnail(&ui, index, &thumbnail);
                                }
                            }
                        }
                    }
                    for offset in 0..(end - start) {
                        if successful_offsets.contains(&offset) {
                            continue;
                        }
                        if let Some(expected_id) = expected_ids.get(start + offset) {
                            let state = no_thumbnail_state(
                                source_available.get(offset).copied().unwrap_or(false),
                            );
                            let items = ui.get_channel_items();
                            if let Some(index) = card_index_by_id(&items, expected_id) {
                                update_thumbnail_state(&ui, index, expected_id, state, true);
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
        assert_eq!(display_title(&item), "Untitled video");
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
    fn stale_thumbnail_results_are_matched_by_id_after_reordering() {
        let model: slint::ModelRc<VideoCardData> = Rc::new(slint::VecModel::from(vec![
            card("item-b", "B"),
            card("item-a", "A"),
        ]))
        .into();
        assert_eq!(card_index_by_id(&model, "item-a"), Some(1));
        assert_eq!(card_index_by_id(&model, "removed-item"), None);
    }
}
