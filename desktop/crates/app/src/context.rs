use crate::phash::PhashCoordinator;
use crate::reporter::PlaybackReporter;
use crate::session::PlaybackSessionController;
use adult_hub_client::{JellyfinClient, ResolverClient, TvItem};
use adult_hub_player::Player;
use adult_hub_storage::{CredentialStore, LocalStore};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Destination for application-wide screen history navigation.
#[derive(Clone, Debug, PartialEq)]
pub enum NavDestination {
    Home,
    Browse {
        category: String,
        search_query: String,
        genre_query: String,
        studio_query: String,
        performer_query: String,
        performer_search_query: String,
        performer_letter: String,
        performers_photo_only: bool,
        drilldown: bool,
    },
    Channels,
    Settings,
    VideoDetails {
        id: String,
    },
    PerformerDetails {
        name: String,
    },
    StudioDetails {
        name: String,
    },
    Player {
        id: String,
    },
}

/// Global application context bundling shared stores, controllers, and state.
#[derive(Clone)]
pub struct AppContext {
    pub player: Option<Arc<Player>>,
    pub headers: Vec<String>,
    pub local_store: Arc<LocalStore>,
    pub credential_store: Arc<CredentialStore>,
    pub jellyfin: Arc<JellyfinClient>,
    pub resolver: Arc<ResolverClient>,
    pub reporter: Arc<PlaybackReporter>,
    pub session_ctrl: Arc<PlaybackSessionController>,
    pub phash_coord: Arc<PhashCoordinator>,
    pub current_playing_item: Arc<Mutex<Option<TvItem>>>,
    pub detail_active_item: Arc<Mutex<Option<TvItem>>>,
    pub pending_resume_position: Arc<Mutex<Option<f64>>>,
    pub cached_catalog: Arc<Mutex<Vec<TvItem>>>,
    pub home_catalog: Arc<Mutex<Vec<TvItem>>>,
    pub performer_catalog: Arc<Mutex<Vec<TvItem>>>,
    pub studio_catalog: Arc<Mutex<Vec<TvItem>>>,
    pub autoplay_active: Arc<AtomicBool>,
    pub hwdec_active: Arc<AtomicBool>,
    pub player_paused_for_details: Arc<AtomicBool>,
    pub is_navigating_back: Arc<AtomicBool>,
    pub nav_history: Arc<Mutex<Vec<NavDestination>>>,
    pub shuffle_engine: Arc<crate::shuffle::ShuffleEngine>,
}

impl AppContext {
    pub fn push_nav_destination(&self, dest: NavDestination) {
        if self.is_navigating_back.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let mut history = self.nav_history.lock().unwrap();
        if history.last() != Some(&dest) {
            history.push(dest);
        }
    }

    pub fn pop_nav_destination(&self) -> Option<NavDestination> {
        self.nav_history.lock().unwrap().pop()
    }

    /// Common item lookup mechanism for Home, Continue Watching, Watch History,
    /// Browse, Details, Channels, and fallback synthesis.
    pub fn find_catalog_item(&self, id: &str) -> Option<TvItem> {
        if let Some(item) = self.detail_active_item.lock().unwrap().as_ref() {
            if item.id == id {
                return Some(item.clone());
            }
        }
        if let Some(item) = self.current_playing_item.lock().unwrap().as_ref() {
            if item.id == id {
                return Some(item.clone());
            }
        }
        if let Some(item) = self.cached_catalog.lock().unwrap().iter().find(|i| i.id == id) {
            return Some(item.clone());
        }
        if let Some(item) = self.home_catalog.lock().unwrap().iter().find(|i| i.id == id) {
            return Some(item.clone());
        }
        if let Some(item) = self.performer_catalog.lock().unwrap().iter().find(|i| i.id == id) {
            return Some(item.clone());
        }
        if let Some(item) = self.studio_catalog.lock().unwrap().iter().find(|i| i.id == id) {
            return Some(item.clone());
        }
        if let Ok(records) = self.local_store.get_watch_history(100) {
            for json_str in records {
                if let Ok(item) = serde_json::from_str::<TvItem>(&json_str) {
                    if item.id == id {
                        return Some(item);
                    }
                }
            }
        }
        None
    }
}
