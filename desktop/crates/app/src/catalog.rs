use crate::{DiscoveredServerItem, MainWindow, VideoCardData};
use adult_hub_client::{TvItem, TvPersonRole};
use adult_hub_storage::LocalStore;
use std::rc::Rc;

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
            duration: "32:15".into(),
            year: "2026".into(),
            is_played: false,
            provider: "telegram".into(),
            is_favorite: false,
            studio: "4K Studios".into(),
        },
        VideoCardData {
            id: "telegram:channel_vr:2".into(),
            title: "VR Scenes Channel - Volume 3".into(),
            duration: "45:00".into(),
            year: "2026".into(),
            is_played: false,
            provider: "telegram".into(),
            is_favorite: false,
            studio: "VR Scenes".into(),
        },
    ]
}

pub fn set_catalog(ui: &MainWindow, items: Vec<VideoCardData>) {
    let model = Rc::new(slint::VecModel::from(items));
    ui.set_catalog_items(model.into());
}

pub fn set_discovered_servers(ui: &MainWindow, items: Vec<DiscoveredServerItem>) {
    let model = Rc::new(slint::VecModel::from(items));
    ui.set_discovered_servers(model.into());
}

pub fn tv_item_to_card(item: &TvItem) -> VideoCardData {
    let year_str = item.year.map(|y| y.to_string()).unwrap_or_default();
    let duration_str = item.formatted_duration();
    let provider = if item.is_telegram() {
        "telegram".to_string()
    } else if item.id == "sample" || item.id.starts_with('/') || item.id.contains(":\\") {
        "local".to_string()
    } else {
        "jellyfin".to_string()
    };
    VideoCardData {
        id: item.id.clone().into(),
        title: item.title.clone().into(),
        duration: duration_str.into(),
        year: year_str.into(),
        is_played: item.is_played,
        provider: provider.into(),
        is_favorite: item.is_favorite,
        studio: item.studios.first().cloned().unwrap_or_default().into(),
    }
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
            for tag in &item.tags {
                if excluded_tags.iter().any(|ex| ex.eq_ignore_ascii_case(tag)) {
                    return false;
                }
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
