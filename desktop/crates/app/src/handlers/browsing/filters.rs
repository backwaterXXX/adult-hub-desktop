use crate::MainWindow;
use adult_hub_client::TvItem;
use adult_hub_storage::LocalStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Server-side quality tier requested from the catalog API.
///
/// The backend owns the definition (resolution and bitrate); the client must
/// never re-derive quality from file size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum QualityTier {
    #[default]
    All,
    Low,
    Medium,
    High,
}

impl QualityTier {
    /// Value for the `quality` query parameter; `None` means no filter.
    pub(super) fn query(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
        }
    }

    /// Clicking the active chip clears the filter.
    pub(super) fn toggled(self, tier: Self) -> Self {
        if self == tier { Self::All } else { tier }
    }

    /// Index used by the UI chips (0 = no filter).
    pub(super) fn index(self) -> i32 {
        match self {
            Self::All => 0,
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
        }
    }
}

/// Sort order for the performer list. The server owns name/video-count sorting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum PerformerSort {
    #[default]
    NameAsc,
    NameDesc,
    VideoCount,
}

impl PerformerSort {
    /// `(sort, direction)` parameters for `/api/v1/catalog/performers`.
    pub(super) fn query(self) -> (&'static str, &'static str) {
        match self {
            Self::NameAsc => ("name", "asc"),
            Self::NameDesc => ("name", "desc"),
            Self::VideoCount => ("video_count", "desc"),
        }
    }

    pub(super) fn index(self) -> i32 {
        match self {
            Self::NameAsc => 0,
            Self::NameDesc => 1,
            Self::VideoCount => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct CatalogFilters {
    pub(super) sort: u8,
    pub(super) descending: bool,
    pub(super) unwatched: bool,
    pub(super) favorites_only: bool,
    pub(super) quality: QualityTier,
    pub(super) duration: u8,
    pub(super) genre: String,
    pub(super) genre_id: String,
    pub(super) studio: String,
    pub(super) studio_id: String,
    pub(super) performer: String,
    pub(super) performer_id: String,
    pub(super) performer_letter: String,
    pub(super) performer_photo_only: bool,
    pub(super) performer_sort: PerformerSort,
}

#[derive(Clone)]
pub(super) struct CatalogFilterChoice {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) selected: bool,
}

impl Default for CatalogFilters {
    fn default() -> Self {
        Self {
            sort: 0,
            descending: true,
            unwatched: false,
            favorites_only: false,
            quality: QualityTier::All,
            duration: 0,
            genre: String::new(),
            genre_id: String::new(),
            studio: String::new(),
            studio_id: String::new(),
            performer: String::new(),
            performer_id: String::new(),
            performer_letter: String::new(),
            performer_photo_only: false,
            performer_sort: PerformerSort::NameAsc,
        }
    }
}

impl CatalogFilters {
    #[cfg(test)]
    pub(super) fn sort_by(&self) -> &'static str {
        match self.sort {
            1 => "SortName",
            2 => "Runtime",
            _ => "DateCreated",
        }
    }

    pub(super) fn active_filter_count(&self, category: &str) -> i32 {
        if category == "Performers" {
            let mut count = 0;
            if !self.performer_letter.is_empty() {
                count += 1;
            }
            return count;
        }
        let mut count = 0;
        if self.unwatched {
            count += 1;
        }
        if self.favorites_only && category != "Favorites" {
            count += 1;
        }
        if self.quality != QualityTier::All {
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

    pub(super) fn accepts(&self, item: &TvItem) -> bool {
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
        // Quality is resolved by the server (resolution or bitrate); deriving it
        // from the file size here is exactly the bug the server filter replaced.
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

pub(super) fn get_category_filters(
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

pub(super) fn set_category_filters(
    filters_map: &Arc<Mutex<std::collections::HashMap<String, CatalogFilters>>>,
    category: &str,
    filters: CatalogFilters,
) {
    filters_map
        .lock()
        .unwrap()
        .insert(category.to_string(), filters);
}

pub(super) fn sync_catalog_filters_to_ui(
    ui: &MainWindow,
    category: &str,
    filters: &CatalogFilters,
) {
    if category == "Performers" {
        ui.set_performer_letter(filters.performer_letter.clone().into());
        ui.set_performers_photo_only(filters.performer_photo_only);
        ui.set_performer_sort_index(filters.performer_sort.index());
    }
    let (sort, duration) = catalog_filter_labels(filters);
    ui.set_catalog_sort_label(sort.into());
    ui.set_catalog_sort_descending(filters.descending);
    ui.set_catalog_unwatched_only(filters.unwatched);
    ui.set_catalog_favorites_only(filters.favorites_only);
    ui.set_catalog_quality_index(filters.quality.index());
    ui.set_catalog_duration_label(duration.into());
    ui.set_catalog_genre_query(filters.genre.clone().into());
    ui.set_catalog_studio_query(filters.studio.clone().into());
    ui.set_catalog_performer_query(filters.performer.clone().into());
    ui.set_catalog_active_filter_count(filters.active_filter_count(category));
}

pub(super) fn catalog_filter_labels(filters: &CatalogFilters) -> (String, String) {
    let sort = match filters.sort {
        1 => "Title",
        2 => "Duration",
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

/// Key holding the per-category filter map in the local settings store.
pub(super) const CATALOG_FILTERS_SETTING: &str = "catalog_filters_v1";

/// Load the per-category filters saved by the previous run.
pub(super) fn load_catalog_filters(store: &LocalStore) -> HashMap<String, CatalogFilters> {
    store
        .get_setting(CATALOG_FILTERS_SETTING)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<HashMap<String, CatalogFilters>>(&json).ok())
        .unwrap_or_default()
}

/// Persist the per-category filter map; failures are not worth interrupting a click.
pub(super) fn save_catalog_filters(store: &LocalStore, filters: &HashMap<String, CatalogFilters>) {
    if let Ok(json) = serde_json::to_string(filters) {
        let _ = store.set_setting(CATALOG_FILTERS_SETTING, &json);
    }
}

/// Next sort option in the UI cycle, with the direction that order implies.
///
/// 0 = date added (newest first), 1 = title (A-Z), 2 = duration (longest
/// first). There is no rating sort: the API does not expose one, so offering it
/// only produced a silent fallback to "latest".
pub(super) fn next_catalog_sort(current: u8) -> (u8, bool) {
    let next = (current + 1) % 3;
    (next, next != 1)
}
