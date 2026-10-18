use crate::phash::PhashCoordinator;
use crate::reporter::PlaybackReporter;
use crate::session::PlaybackSessionController;
use adult_hub_client::{JellyfinClient, ResolverClient, TvItem};
use adult_hub_player::Player;
use adult_hub_storage::{CredentialStore, LocalStore};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Global application context bundling shared stores, controllers, and state.
#[derive(Clone)]
pub struct AppContext {
    pub player: Option<Rc<Player>>,
    pub headers: Vec<String>,
    pub local_store: Arc<LocalStore>,
    pub credential_store: Arc<CredentialStore>,
    pub jellyfin: Arc<JellyfinClient>,
    pub resolver: Arc<ResolverClient>,
    pub reporter: Arc<PlaybackReporter>,
    pub session_ctrl: Arc<PlaybackSessionController>,
    pub phash_coord: Arc<PhashCoordinator>,
    pub current_playing_item: Arc<Mutex<Option<TvItem>>>,
    pub cached_catalog: Arc<Mutex<Vec<TvItem>>>,
    pub home_catalog: Arc<Mutex<Vec<TvItem>>>,
    pub autoplay_active: Arc<AtomicBool>,
    pub hwdec_active: Arc<AtomicBool>,
}
