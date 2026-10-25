use adult_hub_client::{PlayMethod, TvItem};
use adult_hub_storage::{LocalStore, TelegramProgressRecord};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub const RESUME_THRESHOLD_MIN_MS: u64 = 15_000;
pub const RESUME_EXCLUSION_END_MS: u64 = 30_000;
pub const COMPLETION_FRACTION: f64 = 0.90;

pub struct PlaybackSessionController {
    request_counter: AtomicU64,
    local_store: Arc<LocalStore>,
}

impl PlaybackSessionController {
    pub fn new(local_store: Arc<LocalStore>) -> Self {
        Self {
            request_counter: AtomicU64::new(0),
            local_store,
        }
    }

    pub fn next_request_id(&self) -> u64 {
        self.request_counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    #[allow(dead_code)]
    pub fn current_request_id(&self) -> u64 {
        self.request_counter.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn is_request_current(&self, id: u64) -> bool {
        self.request_counter.load(Ordering::SeqCst) == id
    }

    /// Android baseline threshold: position > 15s and < duration - 30s.
    pub fn should_prompt_resume(resume_ms: u64, duration_ms: u64) -> bool {
        if resume_ms <= RESUME_THRESHOLD_MIN_MS {
            return false;
        }
        if duration_ms > 0 && resume_ms >= duration_ms.saturating_sub(RESUME_EXCLUSION_END_MS) {
            return false;
        }
        true
    }

    /// Android baseline completion: >= 90% duration or EOF.
    pub fn is_playback_completed(position_ms: u64, duration_ms: u64, is_eof: bool) -> bool {
        if is_eof {
            return true;
        }
        if duration_ms > 0 {
            let threshold = (duration_ms as f64 * COMPLETION_FRACTION) as u64;
            return position_ms >= threshold;
        }
        false
    }

    pub fn get_telegram_resume_position(&self, provider_video_id: &str) -> Option<u64> {
        let rec = self
            .local_store
            .get_telegram_progress(provider_video_id)
            .ok()
            .flatten()?;
        if rec.is_played {
            None
        } else {
            Some(rec.position_ms)
        }
    }

    /// Resolve the most accurate resume position for any TvItem, ensuring consistency
    /// between Home screen indicators, Jellyfin user data, and local storage.
    ///
    /// Every path to the same material (Home, Continue Watching, search, details,
    /// shuffle) must yield the same position, so ALL known sources are consulted
    /// and the most advanced one wins: server-reported ticks, server-reported
    /// percentage (`.strm` items often carry only a percentage), the local
    /// progress row keyed by `item.id`, and the Telegram progress row keyed by
    /// the provider id.
    pub fn resolve_item_resume_position(
        local_store: &LocalStore,
        session_ctrl: &PlaybackSessionController,
        item: &TvItem,
    ) -> Option<u64> {
        let duration_ms = item.runtime_ticks.map(|t| (t / 10_000) as u64).unwrap_or(0);

        let mut candidates: Vec<u64> = Vec::new();

        if item.playback_position_ticks > 0 {
            candidates.push((item.playback_position_ticks / 10_000) as u64);
        }
        if let Some(pct) = item.played_percentage {
            if pct > 0.0 && pct < 95.0 {
                // The server usually reports ticks as well, which take priority
                // when the duration is known. Derive from the percentage only
                // when no absolute position came back, and use the local
                // duration row as fallback for items missing RunTimeTicks.
                let basis_ms = if duration_ms > 0 {
                    duration_ms
                } else {
                    local_store
                        .get_progress("local", &item.id)
                        .ok()
                        .flatten()
                        .map(|(_, dur)| dur)
                        .unwrap_or(0)
                };
                if basis_ms > 0 {
                    candidates.push(((pct / 100.0) * basis_ms as f64) as u64);
                }
            }
        }
        if let Some(pos) = local_store
            .get_progress("local", &item.id)
            .ok()
            .flatten()
            .map(|(pos, _)| pos)
            .filter(|pos| *pos > 0)
        {
            candidates.push(pos);
        }
        // Telegram rows are keyed by provider id; consult them whenever a
        // provider id exists, regardless of which surface the item came from.
        if let Some(pos) = item.provider_video_id.as_deref().and_then(|prov_id| {
            session_ctrl
                .get_telegram_resume_position(prov_id)
                .or_else(|| session_ctrl.get_telegram_resume_position(&item.id))
        }) {
            if pos > 0 {
                candidates.push(pos);
            }
        }
        if item.is_telegram() {
            if let Some(pos) = session_ctrl.get_telegram_resume_position(&item.id) {
                if pos > 0 {
                    candidates.push(pos);
                }
            }
        }

        candidates.into_iter().max()
    }

    pub fn checkpoint_progress(
        &self,
        item: &TvItem,
        position_ms: u64,
        duration_ms: u64,
        is_ended: bool,
    ) {
        // The local row keyed by item.id is written for EVERY item, so a
        // position saved on one surface is found by every other surface even
        // before the server reports progress back.
        let _ = self
            .local_store
            .save_progress("local", &item.id, position_ms, duration_ms);

        let prov_id = item
            .provider_video_id
            .as_deref()
            .or_else(|| item.id.strip_prefix("telegram:"));
        if !item.is_telegram() || prov_id.is_none() {
            // Jellyfin progress is also reported to server via PlaybackReporter
            return;
        }
        let prov_id = prov_id.unwrap();

        if Self::is_playback_completed(position_ms, duration_ms, is_ended) {
            let _ = self.local_store.mark_telegram_played(
                prov_id,
                true,
                Some(&item.title),
                item.primary_image_tag.as_deref(),
                None,
            );
        } else if position_ms > RESUME_THRESHOLD_MIN_MS {
            let rec = TelegramProgressRecord {
                provider_video_id: prov_id.to_string(),
                channel_id: Some("telegram".into()),
                position_ms,
                duration_ms,
                title: item.title.clone(),
                thumbnail_url: item.primary_image_tag.clone(),
                is_played: false,
                updated_at: 0,
            };
            let _ = self.local_store.save_telegram_progress(&rec);
        }
    }

    pub fn resolve_stream_url(
        item: &TvItem,
        server_url: Option<&str>,
        resolver_url: Option<&str>,
        access_token: Option<&str>,
    ) -> Result<(String, PlayMethod), String> {
        Self::resolve_stream_url_with_variant(item, server_url, resolver_url, access_token, None)
    }

    pub fn resolve_stream_url_with_variant(
        item: &TvItem,
        server_url: Option<&str>,
        resolver_url: Option<&str>,
        access_token: Option<&str>,
        variant: Option<&str>,
    ) -> Result<(String, PlayMethod), String> {
        if item.id == "sample" {
            return Ok((
                crate::options::resolve_sample_media(),
                PlayMethod::DirectPlay,
            ));
        }
        if item.id == "sample2" {
            return Ok((
                crate::options::resolve_named_sample_media("sample2.mp4"),
                PlayMethod::DirectPlay,
            ));
        }

        if item.is_direct_media() {
            return Ok((
                item.id
                    .strip_prefix("direct:")
                    .unwrap_or(&item.id)
                    .to_owned(),
                PlayMethod::DirectPlay,
            ));
        }

        if item.is_telegram() {
            let resolver = resolver_url.unwrap_or("").trim().trim_end_matches('/');
            if resolver.is_empty() {
                return Err("Resolver URL is required to play Telegram content".into());
            }
            let prov_ref = if item.id.starts_with("telegram:") {
                item.id.clone()
            } else if let Some(ref vid) = item.provider_video_id {
                if vid.starts_with("telegram:") {
                    vid.clone()
                } else {
                    format!("telegram:{vid}")
                }
            } else {
                format!("telegram:{}", item.id)
            };
            let variant_param = match variant {
                Some(v) if !v.is_empty() && v != "original" => {
                    format!("&variant={}", adult_hub_client::urlencoding(v))
                }
                _ => String::new(),
            };
            let url = format!("{resolver}/admin/play/{prov_ref}/stream?mode=proxy{variant_param}");
            return Ok((url, PlayMethod::DirectStream));
        }

        let server = server_url.unwrap_or("").trim().trim_end_matches('/');
        if server.is_empty() {
            return Err("Server URL is required to play Jellyfin content".into());
        }
        let token_part = match access_token {
            Some(t) if !t.is_empty() => format!("&api_key={t}"),
            _ => String::new(),
        };
        let url = format!("{server}/Videos/{}/stream?static=true{token_part}", item.id);
        Ok((url, PlayMethod::DirectPlay))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_threshold_semantics() {
        let dur = 100_000; // 100s

        // <= 15s should not prompt resume
        assert!(!PlaybackSessionController::should_prompt_resume(0, dur));
        assert!(!PlaybackSessionController::should_prompt_resume(
            10_000, dur
        ));
        assert!(!PlaybackSessionController::should_prompt_resume(
            15_000, dur
        ));

        // > 15s and < duration - 30s should prompt resume
        assert!(PlaybackSessionController::should_prompt_resume(15_001, dur));
        assert!(PlaybackSessionController::should_prompt_resume(50_000, dur));
        assert!(PlaybackSessionController::should_prompt_resume(69_999, dur));

        // >= duration - 30s (70s) should NOT prompt resume (near end / credits)
        assert!(!PlaybackSessionController::should_prompt_resume(
            70_000, dur
        ));
        assert!(!PlaybackSessionController::should_prompt_resume(
            95_000, dur
        ));

        // Unknown duration: prompts resume as long as > 15s
        assert!(PlaybackSessionController::should_prompt_resume(25_000, 0));
    }

    #[test]
    fn completion_threshold_semantics() {
        let dur = 100_000; // 100s

        assert!(!PlaybackSessionController::is_playback_completed(
            50_000, dur, false
        ));
        assert!(!PlaybackSessionController::is_playback_completed(
            89_900, dur, false
        ));

        // >= 90% marks completed
        assert!(PlaybackSessionController::is_playback_completed(
            90_000, dur, false
        ));
        assert!(PlaybackSessionController::is_playback_completed(
            95_000, dur, false
        ));

        // EOF always marks completed
        assert!(PlaybackSessionController::is_playback_completed(
            10_000, dur, true
        ));
    }

    #[test]
    fn request_counter_invalidation() {
        let store = Arc::new(LocalStore::open_in_memory().unwrap());
        let controller = PlaybackSessionController::new(store);

        let id1 = controller.next_request_id();
        assert!(controller.is_request_current(id1));

        let id2 = controller.next_request_id();
        assert!(!controller.is_request_current(id1));
        assert!(controller.is_request_current(id2));
    }

    fn base_item(id: &str) -> TvItem {
        TvItem {
            id: id.into(),
            title: "Test".into(),
            ..Default::default()
        }
    }

    #[test]
    fn jellyfin_mapped_telegram_item_resumes_from_server_ticks() {
        // Regression: "[Tim Tales] …" showed 59% on Home but plain Play on
        // Details, because a Jellyfin item carrying a `telegram` ProviderId was
        // classified as Telegram and its resume position was read only from the
        // provider-keyed table, which the 15s checkpoint threshold left empty.
        let store = Arc::new(LocalStore::open_in_memory().unwrap());
        let ctrl = PlaybackSessionController::new(store.clone());
        let duration_ms: u64 = 1_418_000; // 1418s
        let duration_ticks = (duration_ms * 10_000) as i64;
        let resume_ms: u64 = 836_620; // ~59%

        let item = TvItem {
            provider_video_id: Some("tg-999".into()),
            runtime_ticks: Some(duration_ticks),
            playback_position_ticks: (resume_ms * 10_000) as i64,
            ..base_item("jf-guid-0001")
        };

        let resume = PlaybackSessionController::resolve_item_resume_position(&store, &ctrl, &item);
        assert!(resume.is_some(), "server ticks must be visible to Details");
        assert_eq!(resume, Some(resume_ms));
        let fraction = resume_ms as f64 / duration_ms as f64;
        assert!((fraction - 0.59).abs() < 0.02, "fraction was {fraction}");
        assert!(PlaybackSessionController::should_prompt_resume(
            resume.unwrap(),
            duration_ms
        ));
    }

    #[test]
    fn early_checkpoint_is_visible_on_every_entry_path() {
        // Regression: a 19s checkpoint used to reach the provider table only
        // past the 15s-plus threshold and never the row Details reads, so the
        // position was visible on one surface and lost on another. The row
        // keyed by item.id is now written for every item and read on every path.
        let store = Arc::new(LocalStore::open_in_memory().unwrap());
        let ctrl = PlaybackSessionController::new(store.clone());

        let mut jf_item = base_item("jf-guid-0002");
        jf_item.runtime_ticks = Some(600_000_000_000); // 600s
        let mut tg_item = base_item("telegram:chan:457");
        tg_item.provider_video_id = Some("chan:457".into());
        tg_item.source_provider = Some("telegram".into());
        tg_item.runtime_ticks = Some(600_000_000_000);

        for item in [&mut jf_item, &mut tg_item] {
            ctrl.checkpoint_progress(item, 19_000, 600_000, false);
            let resume =
                PlaybackSessionController::resolve_item_resume_position(&store, &ctrl, item);
            assert_eq!(resume, Some(19_000), "resume lost for {}", item.id);

            // Same material re-fetched from the server with no progress fields
            // yet (metadata race) must still resolve the local position.
            item.playback_position_ticks = 0;
            item.played_percentage = None;
            let resume =
                PlaybackSessionController::resolve_item_resume_position(&store, &ctrl, item);
            assert_eq!(resume, Some(19_000), "missing metadata zeroed progress");
        }
    }

    #[test]
    fn resume_uses_latest_across_sources_and_percentage_without_duration() {
        let store = Arc::new(LocalStore::open_in_memory().unwrap());
        let ctrl = PlaybackSessionController::new(store.clone());

        // Percentage-only item (.strm without RunTimeTicks): fall back to the
        // locally recorded duration instead of dropping resume entirely.
        store
            .save_progress("local", "strm-item", 20_000, 100_000)
            .unwrap();
        let item = TvItem {
            played_percentage: Some(60.0),
            ..base_item("strm-item")
        };
        assert_eq!(
            PlaybackSessionController::resolve_item_resume_position(&store, &ctrl, &item),
            Some(60_000)
        );

        // Newest position wins between server ticks and local rows.
        let item = TvItem {
            runtime_ticks: Some(1_000_000_000),
            playback_position_ticks: 250_000_000, // 25s
            ..base_item("jf-guid-0003")
        };
        store
            .save_progress("local", "jf-guid-0003", 40_000, 100_000)
            .unwrap();
        assert_eq!(
            PlaybackSessionController::resolve_item_resume_position(&store, &ctrl, &item),
            Some(40_000)
        );
    }

    #[test]
    fn stream_url_resolution() {
        let jf_item = TvItem {
            id: "a1b2c3d4-e5f6-7890-abcd-ef1234567890".into(),
            title: "Movie".into(),
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
            child_count: None,
            is_favorite: false,
            playback_position_ticks: 0,
            played_percentage: None,
            is_played: false,
            provider_video_id: None,
            source_provider: None,
            media_source_id: None,
            container: None,
            file_size_bytes: None,
            is_placeholder_source: false,
            width: None,
            height: None,
            community_rating: None,
        };
        let (url, method) = PlaybackSessionController::resolve_stream_url(
            &jf_item,
            Some("http://jf.local:8096"),
            None,
            Some("my-token"),
        )
        .unwrap();
        assert_eq!(
            url,
            "http://jf.local:8096/Videos/a1b2c3d4-e5f6-7890-abcd-ef1234567890/stream?static=true&api_key=my-token"
        );
        assert_eq!(method, PlayMethod::DirectPlay);

        let mut tg_item = jf_item.clone();
        tg_item.provider_video_id = Some("101".into());
        let (url, method) = PlaybackSessionController::resolve_stream_url(
            &tg_item,
            None,
            Some("http://resolver.local:8080"),
            None,
        )
        .unwrap();
        assert_eq!(
            url,
            "http://resolver.local:8080/admin/play/telegram:101/stream?mode=proxy"
        );
        assert_eq!(method, PlayMethod::DirectStream);

        // Variant resolution
        let (orig_variant_url, _) = PlaybackSessionController::resolve_stream_url_with_variant(
            &tg_item,
            None,
            Some("http://resolver.local:8080"),
            None,
            Some("original"),
        )
        .unwrap();
        assert_eq!(
            orig_variant_url,
            "http://resolver.local:8080/admin/play/telegram:101/stream?mode=proxy"
        );

        let (alt_variant_url, _) = PlaybackSessionController::resolve_stream_url_with_variant(
            &tg_item,
            None,
            Some("http://resolver.local:8080"),
            None,
            Some("alt_720p"),
        )
        .unwrap();
        assert_eq!(
            alt_variant_url,
            "http://resolver.local:8080/admin/play/telegram:101/stream?mode=proxy&variant=alt_720p"
        );
    }
}
