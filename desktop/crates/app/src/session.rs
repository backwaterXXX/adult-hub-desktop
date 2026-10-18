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

    pub fn checkpoint_progress(
        &self,
        item: &TvItem,
        position_ms: u64,
        duration_ms: u64,
        is_ended: bool,
    ) {
        if !item.is_telegram() {
            // Jellyfin progress is reported to server via PlaybackReporter
            return;
        }

        let prov_id = item
            .provider_video_id
            .as_deref()
            .or_else(|| item.id.strip_prefix("telegram:"))
            .unwrap_or(&item.id);

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
        if item.id == "sample" {
            return Ok((
                crate::options::resolve_sample_media(),
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
            let url = format!("{resolver}/admin/play/{prov_ref}/stream?mode=proxy");
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
            width: None,
            height: None,
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
    }
}
