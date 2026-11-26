use crate::MainWindow;
use adult_hub_client::TvItem;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CatalogFilters {
    pub(super) sort: u8,
    pub(super) descending: bool,
    pub(super) unwatched: bool,
    pub(super) favorites_only: bool,
    pub(super) hd_only: bool,
    pub(super) duration: u8,
    pub(super) genre: String,
    pub(super) genre_id: String,
    pub(super) studio: String,
    pub(super) studio_id: String,
    pub(super) performer: String,
    pub(super) performer_id: String,
    pub(super) performer_letter: String,
    pub(super) performer_photo_only: bool,
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
            hd_only: false,
            duration: 0,
            genre: String::new(),
            genre_id: String::new(),
            studio: String::new(),
            studio_id: String::new(),
            performer: String::new(),
            performer_id: String::new(),
            performer_letter: String::new(),
            performer_photo_only: false,
        }
    }
}

impl CatalogFilters {
    #[cfg(test)]
    pub(super) fn sort_by(&self) -> &'static str {
        match self.sort {
            1 => "SortName",
            2 => "Runtime",
            3 => "CommunityRating",
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
    }
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

pub(super) fn catalog_filter_labels(filters: &CatalogFilters) -> (String, String) {
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
