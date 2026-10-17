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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TvItem {
    pub id: String,
    pub title: String,
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
    pub is_favorite: bool,
    pub playback_position_ticks: i64,
    pub played_percentage: Option<f64>,
    pub is_played: bool,
    pub provider_video_id: Option<String>,
    pub media_source_id: Option<String>,
    pub container: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl TvItem {
    pub fn is_telegram(&self) -> bool {
        self.provider_video_id
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty())
            || self.id.starts_with("telegram:")
    }

    pub fn runtime_minutes(&self) -> Option<i64> {
        self.runtime_ticks.map(|ticks| ticks / 10_000_000 / 60)
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


