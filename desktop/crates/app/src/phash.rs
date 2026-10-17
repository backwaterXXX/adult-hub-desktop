use crate::diagnostics::GLOBAL_LOG;
use adult_hub_client::{ResolverClient, ResolverError, TvItem};
use adult_hub_storage::{LocalStore, StoredPhashJob};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::thread;
use std::time::Duration;

static TELEGRAM_VIDEO_ID_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^telegram:-?\d+:\d+:\d{1,20}$").expect("valid telegram regex"));

pub fn adult_hub_video_id(item: &TvItem) -> Option<String> {
    let raw = item
        .provider_video_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .or_else(|| {
            if item.id.starts_with("telegram:") {
                Some(item.id.as_str())
            } else {
                None
            }
        })?;

    let full_id = if raw.starts_with("telegram:") {
        raw.to_string()
    } else {
        format!("telegram:{raw}")
    };

    if TELEGRAM_VIDEO_ID_REGEX.is_match(&full_id) {
        Some(full_id)
    } else {
        None
    }
}

pub fn resolve_phash_duration_us(
    runtime_ticks: Option<i64>,
    player_duration_ms: Option<u64>,
) -> Option<u64> {
    if let Some(ticks) = runtime_ticks {
        if ticks > 0 {
            let us = (ticks / 10) as u64;
            if us > 0 {
                return Some(us);
            }
        }
    }
    if let Some(ms) = player_duration_ms {
        if ms > 0 {
            return Some(ms * 1_000);
        }
    }
    None
}

const SUPPORTED_DECODER_PROFILES: [&str; 2] =
    ["resolver-ffmpeg-stash-v2", "resolver-ffmpeg-v1"];

fn is_active_state(state: &str) -> bool {
    let s = state.to_ascii_lowercase();
    s == "submitting" || s == "queued" || s == "processing"
}

fn is_success_state(state: &str) -> bool {
    let s = state.to_ascii_lowercase();
    s == "done" || s == "completed" || s == "succeeded"
}

fn is_terminal_state(state: &str) -> bool {
    let s = state.to_ascii_lowercase();
    is_success_state(&s) || s == "failed" || s == "cancelled"
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhashUiStatus {
    pub message: String,
    pub terminal: bool,
}

pub struct PhashCoordinator {
    store: Arc<LocalStore>,
    resolver_client: Arc<ResolverClient>,
    resolver_base_url: Arc<Mutex<String>>,
    auth_token: Arc<Mutex<Option<String>>>,
    in_flight: Arc<Mutex<HashSet<String>>>,
    job_statuses: Arc<RwLock<HashMap<String, PhashUiStatus>>>,
    cancelled: Arc<AtomicBool>,
}

impl PhashCoordinator {
    pub fn new(
        store: Arc<LocalStore>,
        resolver_client: Arc<ResolverClient>,
        resolver_base_url: String,
        auth_token: Option<String>,
    ) -> Arc<Self> {
        let coordinator = Arc::new(Self {
            store,
            resolver_client,
            resolver_base_url: Arc::new(Mutex::new(resolver_base_url)),
            auth_token: Arc::new(Mutex::new(auth_token)),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            job_statuses: Arc::new(RwLock::new(HashMap::new())),
            cancelled: Arc::new(AtomicBool::new(false)),
        });

        // Resume active or submitting jobs from storage on startup
        let coord_clone = coordinator.clone();
        thread::Builder::new()
            .name("phash-resume".into())
            .spawn(move || {
                coord_clone.resume_stored_jobs();
            })
            .ok();

        coordinator
    }

    pub fn update_auth(&self, base_url: &str, token: Option<&str>) {
        if let Ok(mut url_lock) = self.resolver_base_url.lock() {
            *url_lock = base_url.to_string();
        }
        if let Ok(mut tok_lock) = self.auth_token.lock() {
            *tok_lock = token.map(str::to_string);
        }
        if token.is_none() {
            // Cancel current polling on logout
            self.cancelled.store(true, Ordering::SeqCst);
            if let Ok(mut in_flight) = self.in_flight.lock() {
                in_flight.clear();
            }
        } else {
            self.cancelled.store(false, Ordering::SeqCst);
        }
    }

    pub fn get_status(&self, video_id: &str) -> Option<PhashUiStatus> {
        self.job_statuses.read().ok()?.get(video_id).cloned()
    }

    fn set_status(&self, video_id: &str, status: PhashUiStatus) {
        if let Ok(mut map) = self.job_statuses.write() {
            map.insert(video_id.to_string(), status);
        }
    }

    fn status_for(record: &StoredPhashJob) -> PhashUiStatus {
        match record.state.to_ascii_lowercase().as_str() {
            "submitting" => PhashUiStatus {
                message: "Sending task to Resolver".into(),
                terminal: false,
            },
            "queued" => PhashUiStatus {
                message: "Video matching queued on Resolver".into(),
                terminal: false,
            },
            "processing" => PhashUiStatus {
                message: "Resolver is processing video matching".into(),
                terminal: false,
            },
            s if is_success_state(s) => PhashUiStatus {
                message: "Video matching complete".into(),
                terminal: true,
            },
            "failed" | "cancelled" => {
                let err_detail = record
                    .error_code
                    .as_deref()
                    .map(|c| format!(" ({c})"))
                    .unwrap_or_default();
                PhashUiStatus {
                    message: format!("Video matching failed{err_detail}"),
                    terminal: true,
                }
            }
            other => PhashUiStatus {
                message: format!("Resolver task state: {other}"),
                terminal: false,
            },
        }
    }

    fn resume_stored_jobs(&self) {
        if let Ok(jobs) = self.store.get_all_phash_jobs() {
            for record in jobs {
                self.set_status(&record.video_id, Self::status_for(&record));
                if record.job_id.is_some() && is_active_state(&record.state) {
                    self.start(record.video_id, None, None, record.duration_us);
                } else if record.state == "submitting" && record.duration_us.is_some() {
                    self.start(record.video_id, None, None, record.duration_us);
                }
            }
        }
    }

    pub fn enqueue_generation(&self, item: &TvItem, observed_duration_ms: Option<u64>) {
        let Some(video_id) = adult_hub_video_id(item) else {
            return;
        };
        self.start(video_id, Some(item.clone()), observed_duration_ms, None);
    }

    fn start(
        &self,
        video_id: String,
        item: Option<TvItem>,
        observed_duration_ms: Option<u64>,
        duration_override_us: Option<u64>,
    ) {
        if let Ok(mut in_flight) = self.in_flight.lock() {
            if !in_flight.insert(video_id.clone()) {
                return; // deduplicate
            }
        }

        let store = self.store.clone();
        let client = self.resolver_client.clone();
        let resolver_url_mutex = self.resolver_base_url.clone();
        let auth_token_mutex = self.auth_token.clone();
        let in_flight_mutex = self.in_flight.clone();
        let statuses_mutex = self.job_statuses.clone();
        let cancelled = self.cancelled.clone();

        thread::Builder::new()
            .name(format!("phash-{}", video_id))
            .spawn(move || {
                let vid = video_id.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Self::process_job(
                        &vid,
                        item.as_ref(),
                        observed_duration_ms,
                        duration_override_us,
                        &store,
                        &client,
                        &resolver_url_mutex,
                        &auth_token_mutex,
                        &statuses_mutex,
                        &cancelled,
                    );
                }));

                if let Err(_panic) = result {
                    GLOBAL_LOG.w("PlaybackPhash", "Job processor thread panicked");
                }

                if let Ok(mut in_flight) = in_flight_mutex.lock() {
                    in_flight.remove(&video_id);
                }
            })
            .ok();
    }

    fn process_job(
        video_id: &str,
        item: Option<&TvItem>,
        observed_duration_ms: Option<u64>,
        duration_override_us: Option<u64>,
        store: &LocalStore,
        client: &ResolverClient,
        resolver_url_mutex: &Mutex<String>,
        auth_token_mutex: &Mutex<Option<String>>,
        statuses_mutex: &RwLock<HashMap<String, PhashUiStatus>>,
        cancelled: &AtomicBool,
    ) {
        let stored = store.get_phash_job(video_id).unwrap_or(None);

        if let Some(ref st) = stored {
            if is_active_state(&st.state) {
                if let Some(ref job_id) = st.job_id {
                    Self::poll_status(
                        video_id,
                        job_id,
                        store,
                        client,
                        resolver_url_mutex,
                        auth_token_mutex,
                        statuses_mutex,
                        cancelled,
                    );
                    return;
                }
            }
            if is_terminal_state(&st.state) {
                if let Ok(mut map) = statuses_mutex.write() {
                    map.insert(video_id.to_string(), Self::status_for(st));
                }
                return;
            }
        }

        // If not stored and phash_on_playback setting is false, skip
        let phash_enabled = store
            .get_setting("phash_on_playback")
            .unwrap_or(None)
            .map(|s| s == "true" || s == "1")
            .unwrap_or(false);

        if stored.is_none() && !phash_enabled {
            return;
        }

        let duration_us = duration_override_us
            .or_else(|| stored.as_ref().and_then(|s| s.duration_us))
            .or_else(|| {
                item.and_then(|it| resolve_phash_duration_us(it.runtime_ticks, observed_duration_ms))
            });

        let Some(duration_us) = duration_us else {
            GLOBAL_LOG.i("PlaybackPhash", "skipped: duration unavailable");
            return;
        };

        let token = auth_token_mutex
            .lock()
            .ok()
            .and_then(|t| t.clone())
            .filter(|t| !t.is_empty());

        let Some(token) = token else {
            if let Ok(mut map) = statuses_mutex.write() {
                map.insert(
                    video_id.to_string(),
                    PhashUiStatus {
                        message: "Sign in to queue video matching".into(),
                        terminal: false,
                    },
                );
            }
            return;
        };

        let resolver_url = resolver_url_mutex
            .lock()
            .ok()
            .map(|u| u.clone())
            .unwrap_or_default();

        if resolver_url.is_empty() {
            if let Ok(mut map) = statuses_mutex.write() {
                map.insert(
                    video_id.to_string(),
                    PhashUiStatus {
                        message: "Set a Resolver URL to queue video matching".into(),
                        terminal: false,
                    },
                );
            }
            return;
        }

        if let Ok(mut map) = statuses_mutex.write() {
            map.insert(
                video_id.to_string(),
                PhashUiStatus {
                    message: "Sending task to Resolver".into(),
                    terminal: false,
                },
            );
        }

        let _ = store.save_phash_job(&StoredPhashJob {
            video_id: video_id.to_string(),
            job_id: None,
            state: "submitting".into(),
            error_code: None,
            duration_us: Some(duration_us),
            updated_at: 0,
        });

        // Attempt up to 3 times with exponential delay
        let mut last_error_code: Option<u16> = None;
        for attempt in 0..3 {
            if cancelled.load(Ordering::SeqCst) {
                return;
            }

            match client.generate_phash(&resolver_url, &token, video_id, duration_us) {
                Ok(response) => {
                    if !response.ok {
                        let _ = store.remove_phash_job(video_id);
                        if let Ok(mut map) = statuses_mutex.write() {
                            map.insert(
                                video_id.to_string(),
                                PhashUiStatus {
                                    message: "Resolver rejected the video matching task".into(),
                                    terminal: false,
                                },
                            );
                        }
                        return;
                    }

                    if let Some(job_id) = response.job_id {
                        let state = response.state.unwrap_or_else(|| "queued".into());
                        let rec = StoredPhashJob {
                            video_id: video_id.to_string(),
                            job_id: Some(job_id.clone()),
                            state: state.clone(),
                            error_code: response.error_code,
                            duration_us: Some(duration_us),
                            updated_at: 0,
                        };
                        let _ = store.save_phash_job(&rec);

                        if response.already_computed || is_success_state(&state) {
                            let done_rec = StoredPhashJob {
                                state: "done".into(),
                                ..rec
                            };
                            let _ = store.save_phash_job(&done_rec);
                            if let Ok(mut map) = statuses_mutex.write() {
                                map.insert(
                                    video_id.to_string(),
                                    PhashUiStatus {
                                        message: "Video matching complete".into(),
                                        terminal: true,
                                    },
                                );
                            }
                        } else if is_active_state(&state) || state.is_empty() {
                            Self::poll_status(
                                video_id,
                                &job_id,
                                store,
                                client,
                                resolver_url_mutex,
                                auth_token_mutex,
                                statuses_mutex,
                                cancelled,
                            );
                        } else if let Ok(mut map) = statuses_mutex.write() {
                            map.insert(video_id.to_string(), Self::status_for(&rec));
                        }
                        return;
                    } else if response.already_computed
                        || response
                            .state
                            .as_deref()
                            .is_some_and(|s| s.eq_ignore_ascii_case("done"))
                    {
                        let _ = store.save_phash_job(&StoredPhashJob {
                            video_id: video_id.to_string(),
                            job_id: None,
                            state: "done".into(),
                            error_code: None,
                            duration_us: Some(duration_us),
                            updated_at: 0,
                        });
                        if let Ok(mut map) = statuses_mutex.write() {
                            map.insert(
                                video_id.to_string(),
                                PhashUiStatus {
                                    message: "Video matching complete".into(),
                                    terminal: true,
                                },
                            );
                        }
                        return;
                    } else {
                        let _ = store.remove_phash_job(video_id);
                        if let Ok(mut map) = statuses_mutex.write() {
                            map.insert(
                                video_id.to_string(),
                                PhashUiStatus {
                                    message: "Resolver returned no task ID".into(),
                                    terminal: false,
                                },
                            );
                        }
                        return;
                    }
                }
                Err(ResolverError::Auth(code)) => {
                    let _ = store.remove_phash_job(video_id);
                    if let Ok(mut map) = statuses_mutex.write() {
                        map.insert(
                            video_id.to_string(),
                            PhashUiStatus {
                                message: "Jellyfin authorization was rejected".into(),
                                terminal: false,
                            },
                        );
                    }
                    GLOBAL_LOG.i(
                        "PlaybackPhash",
                        &format!("enqueue request failed (HTTP {code})"),
                    );
                    return;
                }
                Err(ResolverError::Conflict) => {
                    let _ = store.save_phash_job(&StoredPhashJob {
                        video_id: video_id.to_string(),
                        job_id: None,
                        state: "failed".into(),
                        error_code: Some("terminal_job".into()),
                        duration_us: Some(duration_us),
                        updated_at: 0,
                    });
                    if let Ok(mut map) = statuses_mutex.write() {
                        map.insert(
                            video_id.to_string(),
                            PhashUiStatus {
                                message:
                                    "This video matching task failed earlier; retry it from Resolver tasks"
                                        .into(),
                                terminal: true,
                            },
                        );
                    }
                    return;
                }
                Err(e) => {
                    if let ResolverError::Status(c, _) = e {
                        last_error_code = Some(c);
                    }
                    if !e.is_retryable() {
                        let _ = store.remove_phash_job(video_id);
                        if let Ok(mut map) = statuses_mutex.write() {
                            map.insert(
                                video_id.to_string(),
                                PhashUiStatus {
                                    message: format!("Resolver rejected the task ({e})"),
                                    terminal: false,
                                },
                            );
                        }
                        return;
                    }
                    GLOBAL_LOG.i("PlaybackPhash", &format!("enqueue request retry: {e}"));
                }
            }

            if attempt < 2 {
                thread::sleep(Duration::from_millis(1000 * (attempt + 1) as u64));
            }
        }

        let msg = if let Some(c) = last_error_code {
            format!("Resolver unavailable (HTTP {c}); task will retry when opened again")
        } else {
            "Resolver unavailable; task will retry when opened again".into()
        };

        if let Ok(mut map) = statuses_mutex.write() {
            map.insert(
                video_id.to_string(),
                PhashUiStatus {
                    message: msg,
                    terminal: false,
                },
            );
        }
    }

    fn poll_status(
        video_id: &str,
        job_id: &str,
        store: &LocalStore,
        client: &ResolverClient,
        resolver_url_mutex: &Mutex<String>,
        auth_token_mutex: &Mutex<Option<String>>,
        statuses_mutex: &RwLock<HashMap<String, PhashUiStatus>>,
        cancelled: &AtomicBool,
    ) {
        let mut backoff = Duration::from_secs(5);
        let max_backoff = Duration::from_secs(60);

        loop {
            if cancelled.load(Ordering::SeqCst) {
                return;
            }

            thread::sleep(backoff);

            if cancelled.load(Ordering::SeqCst) {
                return;
            }

            let token = auth_token_mutex
                .lock()
                .ok()
                .and_then(|t| t.clone())
                .filter(|t| !t.is_empty());

            let Some(token) = token else {
                if let Ok(mut map) = statuses_mutex.write() {
                    map.insert(
                        video_id.to_string(),
                        PhashUiStatus {
                            message: "Sign in again to check video matching".into(),
                            terminal: false,
                        },
                    );
                }
                return;
            };

            let resolver_url = resolver_url_mutex
                .lock()
                .ok()
                .map(|u| u.clone())
                .unwrap_or_default();

            if resolver_url.is_empty() {
                return;
            }

            match client.get_phash_job(&resolver_url, &token, job_id) {
                Ok(response) => {
                    let Some(job) = response.job else {
                        if let Ok(mut map) = statuses_mutex.write() {
                            map.insert(
                                video_id.to_string(),
                                PhashUiStatus {
                                    message: "Resolver returned an invalid task status".into(),
                                    terminal: false,
                                },
                            );
                        }
                        return;
                    };

                    if job.id != job_id || job.video_id != video_id {
                        if let Ok(mut map) = statuses_mutex.write() {
                            map.insert(
                                video_id.to_string(),
                                PhashUiStatus {
                                    message: "Resolver returned mismatched task status".into(),
                                    terminal: false,
                                },
                            );
                        }
                        return;
                    }

                    if let Some(ref profile) = job.decoder_profile {
                        if !SUPPORTED_DECODER_PROFILES.contains(&profile.as_str()) {
                            if let Ok(mut map) = statuses_mutex.write() {
                                map.insert(
                                    video_id.to_string(),
                                    PhashUiStatus {
                                        message: "Resolver task uses an unsupported decoder profile"
                                            .into(),
                                        terminal: false,
                                    },
                                );
                            }
                            return;
                        }
                    }

                    let state = job.state.to_ascii_lowercase();
                    let record = StoredPhashJob {
                        video_id: video_id.to_string(),
                        job_id: Some(job_id.to_string()),
                        state: state.clone(),
                        error_code: job.error_code,
                        duration_us: None,
                        updated_at: 0,
                    };
                    let _ = store.save_phash_job(&record);

                    if is_success_state(&state) {
                        let _ = store.save_phash_job(&StoredPhashJob {
                            state: "done".into(),
                            ..record
                        });
                        if let Ok(mut map) = statuses_mutex.write() {
                            map.insert(
                                video_id.to_string(),
                                PhashUiStatus {
                                    message: "Video matching complete".into(),
                                    terminal: true,
                                },
                            );
                        }
                        return;
                    } else if state == "failed" || state == "cancelled" {
                        if let Ok(mut map) = statuses_mutex.write() {
                            map.insert(video_id.to_string(), Self::status_for(&record));
                        }
                        return;
                    } else if is_active_state(&state) {
                        if let Ok(mut map) = statuses_mutex.write() {
                            map.insert(video_id.to_string(), Self::status_for(&record));
                        }
                        backoff = Duration::from_secs(5); // reset backoff
                    } else {
                        if let Ok(mut map) = statuses_mutex.write() {
                            map.insert(
                                video_id.to_string(),
                                PhashUiStatus {
                                    message: format!("Resolver task state: {state}"),
                                    terminal: false,
                                },
                            );
                        }
                        backoff = Duration::from_secs(5);
                    }
                }
                Err(ResolverError::Auth(code)) => {
                    if let Ok(mut map) = statuses_mutex.write() {
                        map.insert(
                            video_id.to_string(),
                            PhashUiStatus {
                                message: format!(
                                    "Jellyfin authorization was rejected (HTTP {code}); sign in again"
                                ),
                                terminal: false,
                            },
                        );
                    }
                    return;
                }
                Err(ResolverError::NotFound) => {
                    let _ = store.remove_phash_job(video_id);
                    if let Ok(mut map) = statuses_mutex.write() {
                        map.insert(
                            video_id.to_string(),
                            PhashUiStatus {
                                message: "Resolver task is no longer available".into(),
                                terminal: false,
                            },
                        );
                    }
                    return;
                }
                Err(e) => {
                    if !e.is_retryable() {
                        if let Ok(mut map) = statuses_mutex.write() {
                            map.insert(
                                video_id.to_string(),
                                PhashUiStatus {
                                    message: format!("Could not check Resolver task ({e})"),
                                    terminal: false,
                                },
                            );
                        }
                        return;
                    }
                    GLOBAL_LOG.i(
                        "PlaybackPhash",
                        &format!("status request failed ({e}); retrying with backoff"),
                    );
                    if let Ok(mut map) = statuses_mutex.write() {
                        map.insert(
                            video_id.to_string(),
                            PhashUiStatus {
                                message: "Resolver unavailable; retrying status check".into(),
                                terminal: false,
                            },
                        );
                    }
                    backoff = (backoff * 2).min(max_backoff);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_adult_hub_video_id() {
        let mut item = TvItem {
            id: "telegram:-100123456789:456:789".into(),
            title: "Test".into(),
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
            is_favorite: false,
            playback_position_ticks: 0,
            played_percentage: None,
            is_played: false,
            provider_video_id: None,
            media_source_id: None,
            container: None,
            file_size_bytes: None,
            width: None,
            height: None,
        };

        // Matching id
        assert_eq!(
            adult_hub_video_id(&item).as_deref(),
            Some("telegram:-100123456789:456:789")
        );

        // provider_video_id takes precedence and auto-prepends telegram:
        item.provider_video_id = Some("-100987:111:222".into());
        assert_eq!(
            adult_hub_video_id(&item).as_deref(),
            Some("telegram:-100987:111:222")
        );

        // Invalid format
        item.provider_video_id = Some("invalid_video_id".into());
        item.id = "some-jellyfin-uuid".into();
        assert_eq!(adult_hub_video_id(&item), None);
    }

    #[test]
    fn resolves_phash_duration_precedence() {
        // 1 tick = 100ns -> 10 ticks = 1us.
        // 120_000_000 ticks = 12_000_000 us = 12s.
        assert_eq!(
            resolve_phash_duration_us(Some(120_000_000), Some(20_000)),
            Some(12_000_000)
        );

        // Fallback to player duration in ms * 1000
        assert_eq!(
            resolve_phash_duration_us(None, Some(35_000)),
            Some(35_000_000)
        );
        assert_eq!(
            resolve_phash_duration_us(Some(0), Some(35_000)),
            Some(35_000_000)
        );
        assert_eq!(resolve_phash_duration_us(None, None), None);
    }

    #[test]
    fn status_formatting_records() {
        let queued = StoredPhashJob {
            video_id: "v1".into(),
            job_id: Some("j1".into()),
            state: "queued".into(),
            error_code: None,
            duration_us: None,
            updated_at: 0,
        };
        assert_eq!(
            PhashCoordinator::status_for(&queued),
            PhashUiStatus {
                message: "Video matching queued on Resolver".into(),
                terminal: false,
            }
        );

        let done = StoredPhashJob {
            video_id: "v1".into(),
            job_id: Some("j1".into()),
            state: "done".into(),
            error_code: None,
            duration_us: None,
            updated_at: 0,
        };
        assert_eq!(
            PhashCoordinator::status_for(&done),
            PhashUiStatus {
                message: "Video matching complete".into(),
                terminal: true,
            }
        );

        let failed = StoredPhashJob {
            video_id: "v1".into(),
            job_id: Some("j1".into()),
            state: "failed".into(),
            error_code: Some("unsupported_codec".into()),
            duration_us: None,
            updated_at: 0,
        };
        assert_eq!(
            PhashCoordinator::status_for(&failed),
            PhashUiStatus {
                message: "Video matching failed (unsupported_codec)".into(),
                terminal: true,
            }
        );
    }
}
