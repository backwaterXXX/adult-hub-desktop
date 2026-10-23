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
            id: "sample2".into(),
            title: "Local Demo Sample 2 (880Hz Tone)".into(),
            overview: Some("Local MP4 test sample 2".into()),
            year: Some(2026),
            runtime_ticks: Some(150_000_000), // 15s
            genres: vec!["Sample".into()],
            studios: vec!["Local".into()],
            container: Some("mp4".into()),
            file_size_bytes: Some(350_000),
            width: Some(1280),
            height: Some(720),
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
    let cols = cols_per_row.clamp(1, 8);
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
            has_4: false,
            card_4: chunk.get(4).cloned().unwrap_or_default(),
            has_5: false,
            card_5: chunk.get(5).cloned().unwrap_or_default(),
            has_6: false,
            card_6: chunk.get(6).cloned().unwrap_or_default(),
            has_7: false,
            card_7: chunk.get(7).cloned().unwrap_or_default(),
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
        if chunk.len() > 4 {
            row.has_4 = true;
        }
        if chunk.len() > 5 {
            row.has_5 = true;
        }
        if chunk.len() > 6 {
            row.has_6 = true;
        }
        if chunk.len() > 7 {
            row.has_7 = true;
        }
        rows.push(row);
    }
    rows
}

pub fn set_catalog(ui: &MainWindow, items: Vec<VideoCardData>) {
    ui.set_catalog_has_more(false);
    ui.set_catalog_scroll_y(0.0);
    ui.set_catalog_anchor_index(0);
    ui.set_catalog_anchor_screen_y(0.0);
    ui.set_catalog_anchor_pending(false);
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
    ui.set_channel_has_more(false);
    ui.set_channel_scroll_y(0.0);
    ui.set_channel_anchor_index(0);
    ui.set_channel_anchor_screen_y(0.0);
    ui.set_channel_anchor_pending(false);
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

fn set_row_card(row: &mut VideoCardRowData, column: usize, card: VideoCardData) {
    match column {
        0 => {
            row.has_0 = true;
            row.card_0 = card;
        }
        1 => {
            row.has_1 = true;
            row.card_1 = card;
        }
        2 => {
            row.has_2 = true;
            row.card_2 = card;
        }
        3 => {
            row.has_3 = true;
            row.card_3 = card;
        }
        4 => {
            row.has_4 = true;
            row.card_4 = card;
        }
        5 => {
            row.has_5 = true;
            row.card_5 = card;
        }
        6 => {
            row.has_6 = true;
            row.card_6 = card;
        }
        7 => {
            row.has_7 = true;
            row.card_7 = card;
        }
        _ => unreachable!("grid rows have at most eight cards"),
    }
}

fn append_grid_page(
    cards_model: &slint::ModelRc<VideoCardData>,
    rows_model: &slint::ModelRc<VideoCardRowData>,
    cards: Vec<VideoCardData>,
    columns: usize,
) -> bool {
    let Some(cards_vec) = cards_model
        .as_any()
        .downcast_ref::<slint::VecModel<VideoCardData>>()
    else {
        return false;
    };
    let Some(rows_vec) = rows_model
        .as_any()
        .downcast_ref::<slint::VecModel<VideoCardRowData>>()
    else {
        return false;
    };

    let old_len = cards_vec.row_count();
    if cards.len() < old_len
        || (0..old_len).any(|index| {
            cards_vec
                .row_data(index)
                .as_ref()
                .map(|card| card.id.as_str())
                != cards.get(index).map(|card| card.id.as_str())
        })
    {
        return false;
    }

    let columns = columns.clamp(1, 8);
    let appended = cards
        .into_iter()
        .skip(old_len)
        .enumerate()
        .map(|(offset, mut card)| {
            card.card_index = (old_len + offset) as i32;
            card
        })
        .collect::<Vec<_>>();
    if appended.is_empty() {
        return true;
    }

    let fill_tail = if old_len > 0 && old_len % columns != 0 {
        let row_index = old_len / columns;
        let Some(mut row) = rows_vec.row_data(row_index) else {
            return false;
        };
        let start_column = old_len % columns;
        let fill = (columns - start_column).min(appended.len());
        for (offset, card) in appended.iter().take(fill).enumerate() {
            set_row_card(&mut row, start_column + offset, card.clone());
        }
        rows_vec.set_row_data(row_index, row);
        fill
    } else {
        0
    };

    cards_vec.extend_from_slice(&appended);

    for chunk in appended[fill_tail..].chunks(columns) {
        rows_vec.push(cards_to_rows(chunk, columns).remove(0));
    }
    true
}

pub fn append_catalog_page(ui: &MainWindow, cards: Vec<VideoCardData>) {
    if !append_grid_page(
        &ui.get_catalog_items(),
        &ui.get_catalog_rows(),
        cards.clone(),
        ui.get_catalog_column_count() as usize,
    ) {
        let previous = ui.get_catalog_items();
        let anchor_id = previous
            .row_data(ui.get_catalog_anchor_index().max(0) as usize)
            .map(|card| card.id);
        let anchor_screen_y = ui.get_catalog_anchor_screen_y();
        set_catalog(ui, cards);
        if let Some(anchor_id) = anchor_id
            && let Some(index) = (0..ui.get_catalog_items().row_count()).find(|index| {
                ui.get_catalog_items()
                    .row_data(*index)
                    .is_some_and(|card| card.id == anchor_id)
            })
        {
            ui.set_catalog_anchor_index(index as i32);
            ui.set_catalog_anchor_screen_y(anchor_screen_y);
            ui.set_catalog_anchor_pending(true);
        }
    }
}

pub fn append_channel_page(ui: &MainWindow, cards: Vec<VideoCardData>) {
    if !append_grid_page(
        &ui.get_channel_items(),
        &ui.get_channel_rows(),
        cards.clone(),
        ui.get_channel_column_count() as usize,
    ) {
        let previous = ui.get_channel_items();
        let anchor_id = previous
            .row_data(ui.get_channel_anchor_index().max(0) as usize)
            .map(|card| card.id);
        let anchor_screen_y = ui.get_channel_anchor_screen_y();
        set_channel_catalog(ui, cards);
        if let Some(anchor_id) = anchor_id
            && let Some(index) = (0..ui.get_channel_items().row_count()).find(|index| {
                ui.get_channel_items()
                    .row_data(*index)
                    .is_some_and(|card| card.id == anchor_id)
            })
        {
            ui.set_channel_anchor_index(index as i32);
            ui.set_channel_anchor_screen_y(anchor_screen_y);
            ui.set_channel_anchor_pending(true);
        }
    }
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
            3 => row.card_3 = card,
            4 => row.card_4 = card,
            5 => row.card_5 = card,
            6 => row.card_6 = card,
            _ => row.card_7 = card,
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
            3 => row.card_3 = card,
            4 => row.card_4 = card,
            5 => row.card_5 = card,
            6 => row.card_6 = card,
            _ => row.card_7 = card,
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
            3 => row.card_3 = card,
            4 => row.card_4 = card,
            5 => row.card_5 = card,
            6 => row.card_6 = card,
            _ => row.card_7 = card,
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
            if !thumbnail_request_matches_card(request, &card) {
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

pub fn is_decorative_emoji(c: char) -> bool {
    let u = c as u32;
    matches!(
        u,
        0x1F300..=0x1FAFF
            | 0x2600..=0x27BF
            | 0xFE00..=0xFE0F
    )
}

pub fn extract_title_tags_and_clean_title(raw_title: &str) -> (String, Vec<String>) {
    let mut extracted_tags = Vec::new();
    let mut cleaned_words = Vec::new();

    for word in raw_title.split_whitespace() {
        let trimmed_word = word.trim_matches(|c: char| is_decorative_emoji(c));
        if trimmed_word.starts_with('#') && trimmed_word.len() > 1 {
            let tag_candidate = trimmed_word
                .trim_start_matches('#')
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if (tag_candidate.chars().any(|c| c.is_alphabetic())
                || tag_candidate.eq_ignore_ascii_case("4k")
                || tag_candidate.eq_ignore_ascii_case("8k")
                || tag_candidate.eq_ignore_ascii_case("vr"))
                && !tag_candidate.is_empty()
            {
                let tag_lower = tag_candidate.to_lowercase();
                if !extracted_tags.contains(&tag_lower) {
                    extracted_tags.push(tag_lower);
                }
                continue;
            }
        }
        if !word.is_empty() && word.chars().all(is_decorative_emoji) {
            continue;
        }
        let cleaned = word.trim_matches(is_decorative_emoji);
        if !cleaned.is_empty() {
            cleaned_words.push(cleaned);
        }
    }

    let mut clean_title = cleaned_words.join(" ");
    clean_title = clean_title
        .trim_matches(|c: char| is_decorative_emoji(c) || c == '-' || c == '|' || c == ':')
        .trim()
        .to_string();

    if clean_title.is_empty() {
        clean_title = raw_title.trim().to_string();
    }

    (clean_title, extracted_tags)
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
        let (cleaned, _) = extract_title_tags_and_clean_title(title);
        if let Some(pos) = cleaned.rfind('.') {
            let ext = &cleaned[pos + 1..];
            if matches!(
                ext.to_ascii_lowercase().as_str(),
                "mp4" | "mkv" | "avi" | "mov" | "webm" | "ts" | "m4v" | "wmv"
            ) {
                let stem = &cleaned[..pos];
                if !stem.is_empty() && !is_hash(stem) {
                    return stem.replace(['_', '.'], " ").trim().to_string();
                }
            }
        }
        if !cleaned.is_empty() && !is_hash(&cleaned) {
            return cleaned;
        }
    }

    // 1. Czytelna nazwa z metadanych (np. wykonawcy, studio, rok)
    if !item.people.is_empty() {
        let performers: Vec<String> = item
            .people
            .iter()
            .filter(|p| {
                p.person_type
                    .as_deref()
                    .is_none_or(|k| k.eq_ignore_ascii_case("actor"))
            })
            .take(2)
            .map(|p| p.name.clone())
            .collect();
        if !performers.is_empty() {
            let perf_str = performers.join(" & ");
            if let Some(studio) = item.studios.first().filter(|s| !s.trim().is_empty()) {
                return format!("{perf_str} - {studio}");
            }
            if let Some(year) = item.year {
                return format!("{perf_str} ({year})");
            }
            return perf_str;
        }
    }
    if let Some(studio) = item.studios.first().filter(|s| !s.trim().is_empty()) {
        if let Some(year) = item.year {
            return format!("{studio} ({year})");
        }
        return format!("{studio} Video");
    }

    // 2. Sensowna nazwa pliku z item.id, provider_video_id lub ścieżki
    for source in [&item.id, item.provider_video_id.as_deref().unwrap_or("")] {
        let source_id = source.strip_prefix("direct:").unwrap_or(source);
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
    }

    // 3. Kanał lub źródło wraz z datą albo innym dostępnym kontekstem
    if item.is_telegram() || item.id.starts_with("telegram:") {
        let parts: Vec<&str> = item.id.split(':').collect();
        if parts.len() >= 3 {
            let channel_raw = parts[1].replace(['_', '-'], " ");
            let channel = titlecase(&channel_raw);
            let msg_id = parts[2];
            if !channel.is_empty() {
                if let Some(year) = item.year {
                    return format!("{channel} ({year}) #{msg_id}");
                }
                return format!("{channel} #{msg_id}");
            }
        }
    }
    if let Some(provider) = item.source_provider.as_deref() {
        if let Some(year) = item.year {
            return format!("{provider} ({year})");
        }
    }

    // 4. Neutralna nazwa z krótkim rozróżniającym oznaczeniem jako ostateczność
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
    } else if item.id == "sample"
        || item.id == "sample2"
        || item.id.starts_with('/')
        || item.id.contains(":\\")
    {
        "local".to_string()
    } else {
        "jellyfin".to_string()
    };
    // Video progress must be computed from actual playback position & duration:
    // Do NOT let the watched/played flag automatically force 100% completed if the item
    // currently has a valid partial resume position (e.g. 2:21 into a 10m video).
    let (progress_val, is_played) = if item.playback_position_ticks > 0
        && item.runtime_ticks.unwrap_or(0) > 0
    {
        let fraction = (item.playback_position_ticks as f32) / (item.runtime_ticks.unwrap() as f32);
        let clamped = fraction.clamp(0.0, 1.0);
        if clamped >= 0.90 {
            (1.0, true)
        } else {
            (clamped, false)
        }
    } else if let Some(pct) = item.played_percentage {
        let fraction = (pct as f32 / 100.0).clamp(0.0, 1.0);
        if fraction >= 0.90 {
            (1.0, true)
        } else if fraction > 0.0 {
            (fraction, false)
        } else {
            (0.0, item.is_played)
        }
    } else if item.is_played {
        (1.0, true)
    } else {
        (0.0, false)
    };

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
    // Cache entries are keyed by the actual source URL. Falling back to the
    // item ID can reuse an image cached by an older mapping even when this
    // record currently has no matching artwork source.
    let cache_key = if !thumbnail_url.is_empty() {
        Some(thumbnail_url.as_str())
    } else if !fallback_thumbnail_url.is_empty() {
        Some(fallback_thumbnail_url.as_str())
    } else {
        None
    };
    let (has_thumbnail, thumbnail) = if let Some(cache_key) = cache_key {
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
        studio: if item
            .genres
            .iter()
            .any(|tag| tag == "__adult_hub_performer" || tag == "__adult_hub_studio")
        {
            item.child_count
                .filter(|count| *count > 0)
                .map(|count| {
                    if count == 1 {
                        "1 movie".to_string()
                    } else {
                        format!("{count} movies")
                    }
                })
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
        let is_performer = item.genres.iter().any(|tag| tag == "__adult_hub_performer");
        // Actor portraits use one shared width everywhere (catalog shelf,
        // profile hero, video cast) so every screen hits the same cache key.
        let image_width = if is_performer {
            crate::thumbnails::person_thumbnail_max_width()
        } else {
            crate::thumbnails::thumbnail_max_width()
        };
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
        // Only request image types the server actually tagged. Tag-less URLs
        // are guaranteed 404s and would stall the bounded thumbnail queue.
        let urls: Vec<String> = order
            .into_iter()
            .filter_map(|image_type| {
                let tag = match image_type {
                    "Thumb" => item.thumb_image_tag.as_deref(),
                    "Backdrop" => item.backdrop_image_tag.as_deref(),
                    _ => item.primary_image_tag.as_deref(),
                }
                .filter(|tag| !tag.trim().is_empty())?;
                Some(JellyfinClient::image_url(
                    server_url,
                    &item.id,
                    image_type,
                    image_width,
                    Some(tag),
                ))
            })
            .collect();
        (
            urls.first().cloned(),
            urls.get(1).cloned(),
            urls.get(2).cloned(),
        )
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

fn log_records_without_thumbnail_source(requests: &[ThumbnailRequest]) {
    for request in requests.iter().filter(|request| {
        request.primary_url.is_empty()
            && request.fallback_url.is_empty()
            && request.alternate_url.is_empty()
    }) {
        let short_id = request.id.chars().take(8).collect::<String>();
        crate::diagnostics::GLOBAL_LOG.w(
            "Thumbnail",
            &format!("record {short_id}: source response contained no usable artwork URL"),
        );
    }
}

pub fn refresh_catalog_thumbnails_async(
    ui: slint::Weak<MainWindow>,
    cards: Vec<VideoCardData>,
    token: String,
    _current_items: Arc<Mutex<Vec<TvItem>>>,
    generation: Option<(Arc<AtomicU64>, u64)>,
) {
    let requests = cards.iter().map(ThumbnailRequest::from).collect::<Vec<_>>();
    log_records_without_thumbnail_source(&requests);
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
    log_records_without_thumbnail_source(&requests);
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

pub fn aggregate_studio_items(items: Vec<TvItem>) -> Vec<TvItem> {
    let mut result: Vec<TvItem> = Vec::with_capacity(items.len());
    let mut title_to_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for item in items {
        if !seen_ids.insert(item.id.clone()) {
            continue;
        }

        let norm_key = item.title.trim().to_lowercase();
        if norm_key.is_empty() {
            result.push(item);
            continue;
        }

        if let Some(&existing_idx) = title_to_index.get(&norm_key) {
            let existing = &mut result[existing_idx];
            if existing.primary_image_tag.is_none() && item.primary_image_tag.is_some() {
                existing.primary_image_tag = item.primary_image_tag;
            }
            if existing.backdrop_image_tag.is_none() && item.backdrop_image_tag.is_some() {
                existing.backdrop_image_tag = item.backdrop_image_tag;
            }
            existing.child_count = match (existing.child_count, item.child_count) {
                (Some(c1), Some(c2)) => Some(c1 + c2),
                (Some(c1), None) => Some(c1),
                (None, Some(c2)) => Some(c2),
                (None, None) => None,
            };
            let alias_tag = format!("alias_id:{}", item.id);
            if !existing.tags.contains(&alias_tag) {
                existing.tags.push(alias_tag);
            }
        } else {
            title_to_index.insert(norm_key, result.len());
            result.push(item);
        }
    }

    result
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
    fn test_extract_title_tags_and_clean_decorative_emojis() {
        let raw = "🔥 Cyberpunk Odyssey: Midnight Run 🎬 #cyberpunk #neon #4k";
        let (clean, tags) = extract_title_tags_and_clean_title(raw);
        assert_eq!(clean, "Cyberpunk Odyssey: Midnight Run");
        assert_eq!(tags, vec!["cyberpunk", "neon", "4k"]);

        // Preserves series numbers like Scene #1
        let scene = "Night Drive Scene #1";
        let (clean_scene, tags_scene) = extract_title_tags_and_clean_title(scene);
        assert_eq!(clean_scene, "Night Drive Scene #1");
        assert!(tags_scene.is_empty());

        // Preserves legitimate punctuation and special chars
        let punct = "Rock & Roll: The Movie - Part 2 (Director's Cut)";
        let (clean_punct, tags_punct) = extract_title_tags_and_clean_title(punct);
        assert_eq!(
            clean_punct,
            "Rock & Roll: The Movie - Part 2 (Director's Cut)"
        );
        assert!(tags_punct.is_empty());
    }

    #[test]
    fn test_fallback_title_precedence() {
        // 1. Metadata: performers
        let perf_item = TvItem {
            id: "uuid-123".into(),
            title: "".into(),
            people: vec![make_sample_person("Eva Green")],
            studios: vec!["Lust Cinema".into()],
            ..Default::default()
        };
        assert_eq!(display_title(&perf_item), "Eva Green - Lust Cinema");

        // 2. Sensible filename
        let file_item = TvItem {
            id: "/media/videos/exclusive_special.mp4".into(),
            title: "".into(),
            ..Default::default()
        };
        assert_eq!(display_title(&file_item), "exclusive special");

        // 3. Channel with date
        let ch_item = TvItem {
            id: "telegram:neon_pulse:456".into(),
            title: "".into(),
            year: Some(2026),
            ..Default::default()
        };
        assert_eq!(display_title(&ch_item), "Neon Pulse (2026) #456");

        // 4. Fallback short marker
        let fallback = TvItem {
            id: "2f0a29deadbeef".into(),
            title: "".into(),
            ..Default::default()
        };
        assert_eq!(display_title(&fallback), "Video #2f0a29");
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

    #[test]
    fn test_aggregate_studio_items() {
        let s1 = TvItem {
            id: "studio-1".into(),
            title: "Active Duty".into(),
            child_count: Some(15),
            primary_image_tag: Some("tag-1".into()),
            ..Default::default()
        };
        let s2 = TvItem {
            id: "studio-2".into(),
            title: "active duty".into(),
            child_count: Some(10),
            backdrop_image_tag: Some("backdrop-2".into()),
            ..Default::default()
        };
        let s3 = TvItem {
            id: "studio-1".into(), // Duplicate id
            title: "Active Duty".into(),
            child_count: Some(5),
            ..Default::default()
        };
        let s4 = TvItem {
            id: "studio-4".into(),
            title: "Active Duty Live".into(), // Distinct studio
            child_count: Some(3),
            ..Default::default()
        };

        let aggregated = aggregate_studio_items(vec![s1, s2, s3, s4]);
        assert_eq!(aggregated.len(), 2);
        assert_eq!(aggregated[0].title, "Active Duty");
        assert_eq!(aggregated[0].child_count, Some(25));
        assert_eq!(aggregated[0].primary_image_tag.as_deref(), Some("tag-1"));
        assert_eq!(
            aggregated[0].backdrop_image_tag.as_deref(),
            Some("backdrop-2")
        );
        assert!(
            aggregated[0]
                .tags
                .contains(&"alias_id:studio-2".to_string())
        );
        assert_eq!(aggregated[1].title, "Active Duty Live");
        assert_eq!(aggregated[1].child_count, Some(3));
    }
}
