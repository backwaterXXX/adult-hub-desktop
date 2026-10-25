use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthResult {
    pub access_token: String,
    pub user_id: String,
    pub username: String,
    pub server_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickConnectInit {
    pub code: String,
    pub secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickConnectStatus {
    pub authenticated: bool,
    pub secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TvLibrary {
    pub id: String,
    pub name: String,
    pub collection_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TvPersonRole {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub person_type: Option<String>,
    pub primary_image_tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TvPersonDetails {
    pub id: String,
    pub name: String,
    pub overview: Option<String>,
    pub primary_image_tag: Option<String>,
    pub child_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TvItem {
    pub id: String,
    pub title: String,
    /// Origin reported by a Resolver DTO. Jellyfin API items leave this empty.
    #[serde(default)]
    pub source_provider: Option<String>,
    pub overview: Option<String>,
    pub year: Option<i32>,
    pub runtime_ticks: Option<i64>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub studios: Vec<String>,
    pub people: Vec<TvPersonRole>,
    pub primary_image_tag: Option<String>,
    pub backdrop_image_tag: Option<String>,
    pub thumb_image_tag: Option<String>,
    pub child_count: Option<u32>,
    pub is_favorite: bool,
    pub playback_position_ticks: i64,
    pub played_percentage: Option<f64>,
    pub is_played: bool,
    pub provider_video_id: Option<String>,
    pub media_source_id: Option<String>,
    pub container: Option<String>,
    pub file_size_bytes: Option<i64>,
    /// True when the library entry is a referral such as a `.strm` file. Its
    /// own size is never the size of the media it points at.
    #[serde(default)]
    pub is_placeholder_source: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    #[serde(default)]
    pub community_rating: Option<f32>,
}

/// Shared binary-unit file size formatter. Details, the player stats and the
/// download surfaces all render sizes the same way so one number is described
/// identically everywhere.
pub fn format_media_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

impl TvItem {
    /// Sizes below this are error documents, playlists or `.strm` referral
    /// bodies, never the media itself. Shared by every surface that reports a
    /// file size so Details, the player HUD and downloads agree.
    pub const MIN_PLAUSIBLE_MEDIA_BYTES: u64 = 1024;

    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            ..Default::default()
        }
    }

    /// True when the material is delivered by the Telegram resolver. That is
    /// either a resolver row (`telegram:` id / `telegram` source provider) or a
    /// Jellyfin library entry whose `telegram` ProviderId identifies the media
    /// the `.strm` pointer refers to — such items stream through the resolver,
    /// while their library metadata and watched state stay on Jellyfin.
    pub fn is_telegram(&self) -> bool {
        self.id.starts_with("telegram:")
            || match self.source_provider.as_deref() {
                Some(provider) => provider.eq_ignore_ascii_case("telegram"),
                None => self
                    .provider_video_id
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty()),
            }
    }

    pub fn is_direct_media(&self) -> bool {
        self.id.starts_with("direct:")
    }

    pub fn runtime_minutes(&self) -> Option<i64> {
        self.runtime_ticks.map(|ticks| ticks / 10_000_000 / 60)
    }

    /// Size of the actual media material. A placeholder entry (e.g. `.strm`)
    /// reports its own tiny document size, which must never be shown as the
    /// video's size. Anything below `MIN_PLAUSIBLE_MEDIA_BYTES` is an error
    /// document, a playlist or a referral body — not media — even when the
    /// server omitted the placeholder flag.
    pub fn media_size_bytes(&self) -> Option<i64> {
        if self.is_placeholder_source {
            None
        } else {
            let min = Self::MIN_PLAUSIBLE_MEDIA_BYTES as i64;
            self.file_size_bytes.filter(|size| *size >= min)
        }
    }

    pub fn format_media_size(&self) -> String {
        match self.media_size_bytes() {
            Some(bytes) => format_media_size(bytes.max(0) as u64),
            None => "Unavailable".to_string(),
        }
    }

    pub fn formatted_duration(&self) -> String {
        let Some(minutes) = self.runtime_minutes() else {
            return String::new();
        };
        let hours = minutes / 60;
        let rem_minutes = minutes % 60;
        if hours > 0 {
            format!("{hours}h {rem_minutes:02}m")
        } else {
            format!("{minutes}m")
        }
    }

    pub fn progress_fraction(&self) -> f32 {
        if let Some(pct) = self.played_percentage {
            (pct / 100.0).clamp(0.0, 1.0) as f32
        } else if let Some(ticks) = self.runtime_ticks {
            if ticks > 0 {
                (self.playback_position_ticks as f64 / ticks as f64).clamp(0.0, 1.0) as f32
            } else {
                0.0
            }
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TvItemsPage {
    pub items: Vec<TvItem>,
    pub total_record_count: usize,
    pub start_index: usize,
}

// Resolver DTOs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryDto {
    pub id: String,
    pub title: String,
    pub slug: Option<String>,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoDto {
    pub id: String,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub provider_video_id: String,
    pub title: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub url: String,
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub performers: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    pub views: Option<String>,
    pub published: Option<String>,
    pub description: Option<String>,
    pub file_size: Option<i64>,
}

fn default_provider() -> String {
    "telegram".into()
}

impl From<VideoDto> for TvItem {
    fn from(v: VideoDto) -> Self {
        let is_telegram = v.provider.eq_ignore_ascii_case("telegram");
        let prov_id = if v.provider_video_id.is_empty() {
            v.id.clone()
        } else {
            v.provider_video_id
        };
        let full_id = if is_telegram && !prov_id.starts_with("telegram:") {
            format!("telegram:{prov_id}")
        } else if is_telegram {
            prov_id.clone()
        } else {
            v.id.clone()
        };
        let categories_lower: std::collections::HashSet<String> = v
            .categories
            .iter()
            .map(|c| c.trim().to_lowercase())
            .collect();

        let is_category_term = |name: &str| -> bool {
            let lower = name.trim().to_lowercase();
            if lower.is_empty() || categories_lower.contains(&lower) {
                return true;
            }
            const KNOWN_CATEGORIES: &[&str] = &[
                "locker room",
                "body worship",
                "behind the scenes",
                "compilation",
                "casting",
                "threesome",
                "foursome",
                "solo",
                "interracial",
                "outdoor",
                "public",
                "pov",
                "point of view",
                "parody",
                "fetish",
                "foot fetish",
                "massage",
                "oil massage",
                "deepthroat",
                "blowjob",
                "creampie",
                "facial",
                "masturbation",
                "striptease",
                "bdsm",
                "bondage",
                "roleplay",
                "cosplay",
                "hardcore",
                "softcore",
                "amateur",
                "vintage",
                "retro",
                "uniform",
                "lingerie",
                "stockings",
                "anal",
                "oral",
                "glamour",
                "erotic",
            ];
            KNOWN_CATEGORIES.iter().any(|cat| lower == *cat)
        };

        let people = v
            .performers
            .into_iter()
            .filter(|name| !is_category_term(name))
            .map(|name| TvPersonRole {
                id: name.to_lowercase().replace(' ', "_"),
                name,
                role: Some("Actor".into()),
                person_type: Some("Actor".into()),
                primary_image_tag: None,
            })
            .collect();

        TvItem {
            id: full_id,
            title: v.title,
            source_provider: Some(v.provider),
            overview: v.description,
            year: None,
            runtime_ticks: if v.duration > 0.0 {
                Some((v.duration * 10_000_000.0) as i64)
            } else {
                None
            },
            genres: v.categories,
            tags: vec![],
            studios: vec![],
            people,
            primary_image_tag: v.thumbnail,
            backdrop_image_tag: None,
            thumb_image_tag: None,
            child_count: None,
            is_favorite: false,
            playback_position_ticks: 0,
            played_percentage: None,
            is_played: false,
            provider_video_id: is_telegram.then_some(prov_id),
            media_source_id: None,
            container: Some("mp4".into()),
            file_size_bytes: v.file_size,
            is_placeholder_source: false,
            width: None,
            height: None,
            community_rating: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerformerBioDto {
    pub name: String,
    pub bio: Option<String>,
    pub birth: Option<String>,
    pub nationality: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub photo_url: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramHistoryResponseDto {
    #[serde(default)]
    pub videos: Vec<VideoDto>,
    pub next_offset_id: Option<i64>,
    #[serde(default)]
    pub message_count: usize,
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PlayMethod {
    #[default]
    DirectPlay,
    DirectStream,
    Transcode,
}

/// Server-selected stream and session metadata returned by Jellyfin PlaybackInfo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackResolution {
    pub stream_url: String,
    pub media_source_id: Option<String>,
    pub play_session_id: Option<String>,
    pub required_headers: Vec<String>,
    pub play_method: PlayMethod,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackStartInfo {
    #[serde(rename = "ItemId")]
    pub item_id: String,
    #[serde(rename = "MediaSourceId", skip_serializing_if = "Option::is_none")]
    pub media_source_id: Option<String>,
    #[serde(rename = "PositionTicks")]
    pub position_ticks: i64,
    #[serde(rename = "PlaySessionId", skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
    #[serde(rename = "CanSeek")]
    pub can_seek: bool,
    #[serde(rename = "IsMuted")]
    pub is_muted: bool,
    #[serde(rename = "IsPaused")]
    pub is_paused: bool,
    #[serde(rename = "RepeatMode")]
    pub repeat_mode: String,
    #[serde(rename = "PlaybackOrder")]
    pub playback_order: String,
    #[serde(rename = "PlayMethod")]
    pub play_method: PlayMethod,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackProgressInfo {
    #[serde(rename = "ItemId")]
    pub item_id: String,
    #[serde(rename = "MediaSourceId", skip_serializing_if = "Option::is_none")]
    pub media_source_id: Option<String>,
    #[serde(rename = "PositionTicks")]
    pub position_ticks: i64,
    #[serde(rename = "PlaySessionId", skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
    #[serde(rename = "CanSeek")]
    pub can_seek: bool,
    #[serde(rename = "IsMuted")]
    pub is_muted: bool,
    #[serde(rename = "IsPaused")]
    pub is_paused: bool,
    #[serde(rename = "RepeatMode")]
    pub repeat_mode: String,
    #[serde(rename = "PlaybackOrder")]
    pub playback_order: String,
    #[serde(rename = "PlayMethod")]
    pub play_method: PlayMethod,
}

impl From<PlaybackStartInfo> for PlaybackProgressInfo {
    fn from(start: PlaybackStartInfo) -> Self {
        Self {
            item_id: start.item_id,
            media_source_id: start.media_source_id,
            position_ticks: start.position_ticks,
            play_session_id: start.play_session_id,
            can_seek: start.can_seek,
            is_muted: start.is_muted,
            is_paused: start.is_paused,
            repeat_mode: start.repeat_mode,
            playback_order: start.playback_order,
            play_method: start.play_method,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackStopInfo {
    #[serde(rename = "ItemId")]
    pub item_id: String,
    #[serde(rename = "MediaSourceId", skip_serializing_if = "Option::is_none")]
    pub media_source_id: Option<String>,
    #[serde(rename = "PositionTicks")]
    pub position_ticks: i64,
    #[serde(rename = "PlaySessionId", skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
    #[serde(rename = "Failed")]
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhashGenerationRequest {
    pub video_id: String,
    pub duration_us: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhashGenerationResponse {
    #[serde(default)]
    pub ok: bool,
    pub job_id: Option<String>,
    pub state: Option<String>,
    #[serde(default)]
    pub already_computed: bool,
    #[serde(default)]
    pub thumbnail_ready: bool,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhashJobStatusResponse {
    #[serde(default)]
    pub ok: bool,
    pub job: Option<PhashJobDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhashJobDto {
    pub id: String,
    pub video_id: String,
    pub state: String,
    pub decoder_profile: Option<String>,
    pub error_code: Option<String>,
    #[serde(default)]
    pub thumbnail_ready: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoVariantDto {
    pub id: String,
    #[serde(default)]
    pub is_original: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codec: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<u64>,
    pub duration: Option<f64>,
    pub bitrate: Option<u64>,
    pub estimated_average_bitrate: Option<u64>,
}

impl VideoVariantDto {
    pub fn resolution_label(&self) -> String {
        match self.height {
            Some(2160) => "4K".to_string(),
            Some(1440) => "1440p".to_string(),
            Some(1080) => "1080p".to_string(),
            Some(720) => "720p".to_string(),
            Some(480) => "480p".to_string(),
            Some(360) => "360p".to_string(),
            Some(h) if h > 0 => format!("{h}p"),
            _ => match (self.width, self.height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => format!("{w}x{h}"),
                _ => "Unknown".to_string(),
            },
        }
    }

    pub fn display_label(&self, has_duplicate_resolution: bool) -> String {
        let res = self.resolution_label();
        let size_str = self.file_size.map(format_media_size);
        let codec_str = self.codec.as_deref().map(|c| c.to_uppercase());

        if self.is_original {
            if has_duplicate_resolution {
                let mut extra = Vec::new();
                if let Some(c) = codec_str {
                    extra.push(c);
                }
                if let Some(s) = size_str {
                    extra.push(s);
                }
                if extra.is_empty() {
                    format!("{res} (Original)")
                } else {
                    format!("{res} (Original, {})", extra.join(", "))
                }
            } else if let Some(s) = size_str {
                format!("{res} (Original, {s})")
            } else {
                format!("{res} (Original)")
            }
        } else if has_duplicate_resolution {
            let mut extra = Vec::new();
            if let Some(c) = codec_str {
                extra.push(c);
            }
            if let Some(s) = size_str {
                extra.push(s);
            }
            if extra.is_empty() {
                res
            } else {
                format!("{res} ({})", extra.join(", "))
            }
        } else if let Some(s) = size_str {
            format!("{res} ({s})")
        } else {
            res
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoVariantsResponseDto {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub video_processing_pending: bool,
    #[serde(default)]
    pub has_alternatives: bool,
    pub video_id: Option<String>,
    #[serde(default)]
    pub variants: Vec<VideoVariantDto>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_dto_to_tv_item_conversion() {
        let dto = VideoDto {
            id: "vid_999".into(),
            provider: "telegram".into(),
            provider_video_id: "channel_4k:999".into(),
            title: "Ultra 4K Scene".into(),
            slug: "ultra-4k-scene".into(),
            url: "https://t.me/c/123/999".into(),
            thumbnail: Some("https://thumb/999.jpg".into()),
            duration: 120.0,
            performers: vec!["Eva Green".into(), "Mia Ray".into()],
            categories: vec!["4K".into(), "Exclusive".into()],
            views: Some("15k".into()),
            published: Some("2026-10-15".into()),
            description: Some("Top tier performance".into()),
            file_size: Some(1_048_576),
        };

        let item: TvItem = dto.into();
        assert_eq!(item.id, "telegram:channel_4k:999");
        assert_eq!(item.title, "Ultra 4K Scene");
        assert_eq!(item.overview.as_deref(), Some("Top tier performance"));
        assert_eq!(item.runtime_ticks, Some(1_200_000_000));
        assert_eq!(item.genres, vec!["4K", "Exclusive"]);
        assert_eq!(item.people.len(), 2);
        assert_eq!(item.people[0].name, "Eva Green");
        assert_eq!(item.people[1].name, "Mia Ray");
        assert_eq!(
            item.primary_image_tag.as_deref(),
            Some("https://thumb/999.jpg")
        );
        assert_eq!(item.provider_video_id.as_deref(), Some("channel_4k:999"));
        assert!(item.is_telegram());
    }

    #[test]
    fn non_telegram_resolver_provider_is_not_misclassified_as_telegram() {
        let item: TvItem = VideoDto {
            id: "jellyfin-item-id".into(),
            provider: "jellyfin".into(),
            provider_video_id: "remote-provider-id".into(),
            title: "A real library title".into(),
            slug: String::new(),
            url: String::new(),
            thumbnail: Some("/admin/artwork/poster.jpg".into()),
            duration: 0.0,
            performers: Vec::new(),
            categories: Vec::new(),
            views: None,
            published: None,
            description: None,
            file_size: None,
        }
        .into();

        assert_eq!(item.id, "jellyfin-item-id");
        assert_eq!(item.source_provider.as_deref(), Some("jellyfin"));
        assert_eq!(item.provider_video_id, None);
        assert!(!item.is_telegram());
    }

    #[test]
    fn telegram_record_without_artwork_stays_a_missing_source_not_a_fake_url() {
        let dto: VideoDto = serde_json::from_value(serde_json::json!({
            "id": "resolver-row-17",
            "provider": "telegram",
            "provider_video_id": "-100123:456:789",
            "title": "Example item",
            "thumbnail": null
        }))
        .unwrap();
        let item = TvItem::from(dto);

        assert_eq!(item.id, "telegram:-100123:456:789");
        assert_eq!(item.source_provider.as_deref(), Some("telegram"));
        assert_eq!(item.primary_image_tag, None);
        assert!(item.is_telegram());
    }

    #[test]
    fn tiny_referral_size_is_never_shown_as_media_size() {
        // Regression: "Doctors…" showed "102 B" in Details while the player and
        // download reported ~109 MB — the `.strm` pointer size passed the old
        // `> 0` guard when the server omitted IsPlaceHolder.
        let mut item = TvItem::new("jf-guid", "Doctors");
        item.file_size_bytes = Some(102);
        item.is_placeholder_source = false;
        assert_eq!(item.media_size_bytes(), None);
        assert_eq!(item.format_media_size(), "Unavailable");

        item.file_size_bytes = Some(114_169_800);
        assert_eq!(item.format_media_size(), "108.9 MB");
        item.file_size_bytes = Some(1_073_741_824);
        assert_eq!(item.format_media_size(), "1.00 GB");
        item.file_size_bytes = Some(0);
        assert_eq!(item.format_media_size(), "Unavailable");
    }

    #[test]
    fn parses_video_variants_dto_and_formats_labels() {
        let json = r#"{
            "status": "ready",
            "video_processing_pending": false,
            "has_alternatives": true,
            "video_id": "telegram:-100123:456:789",
            "variants": [
                {
                    "id": "original",
                    "is_original": true,
                    "width": 1920,
                    "height": 1080,
                    "codec": "h264",
                    "mime_type": "video/mp4",
                    "file_size": 2500000000,
                    "duration": 120.0,
                    "estimated_average_bitrate": 8000000
                },
                {
                    "id": "alt_1001",
                    "is_original": false,
                    "width": 1920,
                    "height": 1080,
                    "codec": "hevc",
                    "mime_type": "video/mp4",
                    "file_size": 1100000000,
                    "duration": 120.0,
                    "estimated_average_bitrate": 4000000
                },
                {
                    "id": "alt_1002",
                    "is_original": false,
                    "width": 1280,
                    "height": 720,
                    "codec": "h264",
                    "mime_type": "video/mp4",
                    "file_size": 600000000,
                    "duration": 120.0,
                    "estimated_average_bitrate": 2000000
                }
            ]
        }"#;

        let resp: VideoVariantsResponseDto = serde_json::from_str(json).unwrap();
        assert_eq!(resp.status, "ready");
        assert!(resp.has_alternatives);
        assert!(!resp.video_processing_pending);
        assert_eq!(resp.variants.len(), 3);

        let orig = &resp.variants[0];
        assert_eq!(orig.resolution_label(), "1080p");
        // Duplicate resolution: distinguish with codec and size
        let orig_label = orig.display_label(true);
        assert!(orig_label.contains("1080p"));
        assert!(orig_label.contains("Original"));
        assert!(orig_label.contains("H264"));
        assert!(orig_label.contains("2.33 GB"));

        let alt1 = &resp.variants[1];
        let alt1_label = alt1.display_label(true);
        assert!(alt1_label.contains("1080p"));
        assert!(alt1_label.contains("HEVC"));
        assert!(alt1_label.contains("1.02 GB"));

        let alt2 = &resp.variants[2];
        let alt2_label = alt2.display_label(false);
        assert_eq!(alt2_label, "720p (572.2 MB)");
    }
}
