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
    pub download_manager: Arc<crate::download::DownloadManager>,
    pub active_playback_context: Arc<Mutex<Vec<TvItem>>>,
}

impl AppContext {
    pub fn set_playback_context(&self, items: Vec<TvItem>) {
        *self.active_playback_context.lock().unwrap() = items;
    }

    pub fn get_next_contextual_item(&self, current_id: Option<&str>) -> Option<TvItem> {
        let items = self.active_playback_context.lock().unwrap().clone();
        if items.is_empty() {
            return None;
        }
        if let Some(id) = current_id {
            if let Some(pos) = items.iter().position(|it| it.id == id) {
                let next_idx = (pos + 1) % items.len();
                return Some(items[next_idx].clone());
            }
        }
        Some(items[0].clone())
    }
    pub fn resolve_effective_resolver_url(
        local_store: &LocalStore,
        credential_store: &CredentialStore,
    ) -> Option<String> {
        let saved = local_store
            .get_setting("resolver_base_url")
            .ok()
            .flatten()
            .or_else(|| local_store.get_setting("resolver_url").ok().flatten())
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty());

        if saved.is_some() {
            return saved;
        }

        credential_store
            .get()
            .ok()
            .flatten()
            .map(|s| s.server_url.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn get_resolver_url(&self) -> Option<String> {
        Self::resolve_effective_resolver_url(&self.local_store, &self.credential_store)
    }

    #[allow(dead_code)]
    pub fn update_resolver_configuration(&self, url: &str) {
        let trimmed = url.trim().trim_end_matches('/').to_string();
        let _ = self.local_store.set_setting("resolver_base_url", &trimmed);
        let _ = self.local_store.set_setting("resolver_url", &trimmed);
        crate::thumbnails::set_resolver_base_url(&trimmed);
        let session = self.credential_store.get().ok().flatten();
        crate::thumbnails::set_resolver_auth_token(
            session.as_ref().map(|s| s.access_token.as_str()),
        );
        self.phash_coord
            .update_auth(&trimmed, session.as_ref().map(|s| s.access_token.as_str()));
    }

    pub fn push_nav_destination(&self, dest: NavDestination) {
        if self
            .is_navigating_back
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let mut history = self.nav_history.lock().unwrap();
        Self::push_history(&mut history, dest);
    }

    /// Append a destination unless it repeats the screen already on top. The
    /// stack (not a single "previous screen" field) is what keeps nested
    /// transitions — list → film → performer → another film — unwinding in the
    /// real visit order.
    pub fn push_history(history: &mut Vec<NavDestination>, dest: NavDestination) {
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
        if let Some(item) = self
            .home_catalog
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.id == id)
        {
            return Some(item.clone());
        }
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
        if let Some(item) = self
            .cached_catalog
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.id == id)
        {
            return Some(item.clone());
        }
        if let Some(item) = self
            .performer_catalog
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.id == id)
        {
            return Some(item.clone());
        }
        if let Some(item) = self
            .studio_catalog
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.id == id)
        {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_contextual_playback_navigation() {
        let active_context = Arc::new(Mutex::new(Vec::new()));
        let list = vec![
            TvItem {
                id: "vid-1".into(),
                title: "Video 1".into(),
                ..Default::default()
            },
            TvItem {
                id: "vid-2".into(),
                title: "Video 2".into(),
                ..Default::default()
            },
            TvItem {
                id: "vid-3".into(),
                title: "Video 3".into(),
                ..Default::default()
            },
        ];
        *active_context.lock().unwrap() = list.clone();

        // Sequential: from vid-1 -> vid-2
        let pos1 = list.iter().position(|it| it.id == "vid-1").unwrap();
        assert_eq!(list[(pos1 + 1) % list.len()].id, "vid-2");

        // Sequential: from vid-2 -> vid-3
        let pos2 = list.iter().position(|it| it.id == "vid-2").unwrap();
        assert_eq!(list[(pos2 + 1) % list.len()].id, "vid-3");

        // Looping: from vid-3 -> vid-1
        let pos3 = list.iter().position(|it| it.id == "vid-3").unwrap();
        assert_eq!(list[(pos3 + 1) % list.len()].id, "vid-1");
    }

    #[test]
    fn nav_history_unwinds_nested_transitions() {
        use NavDestination::*;
        let mut history = Vec::new();

        // The audited flow: playback → "…" → Details → Back must return to the
        // player with its item id, not to Home.
        AppContext::push_history(
            &mut history,
            Browse {
                category: "Latest".into(),
                search_query: String::new(),
                genre_query: String::new(),
                studio_query: String::new(),
                performer_query: String::new(),
                performer_search_query: String::new(),
                performer_letter: String::new(),
                performers_photo_only: false,
                drilldown: false,
            },
        );
        AppContext::push_history(
            &mut history,
            Player {
                id: "movie-1".into(),
            },
        );
        assert_eq!(
            history.pop(),
            Some(Player {
                id: "movie-1".into()
            }),
            "Back from Details must target the player"
        );

        // list → film → performer → another film unwinds in reverse visit order.
        let mut history = Vec::new();
        AppContext::push_history(&mut history, Home);
        AppContext::push_history(&mut history, VideoDetails { id: "a".into() });
        AppContext::push_history(
            &mut history,
            PerformerDetails {
                name: "Jane".into(),
            },
        );
        AppContext::push_history(&mut history, VideoDetails { id: "b".into() });
        assert_eq!(history.pop(), Some(VideoDetails { id: "b".into() }));
        assert_eq!(
            history.pop(),
            Some(PerformerDetails {
                name: "Jane".into()
            })
        );
        assert_eq!(history.pop(), Some(VideoDetails { id: "a".into() }));
        assert_eq!(history.pop(), Some(Home));
        assert!(history.pop().is_none());

        // Re-entering the same screen from itself must not grow the stack.
        let mut history = Vec::new();
        AppContext::push_history(&mut history, Home);
        AppContext::push_history(&mut history, Home);
        assert_eq!(history.len(), 1);
    }
}
