use crate::models::{TvItem, TvPersonRole};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Deserialize)]
pub(super) struct ProblemDetails {
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) detail: Option<String>,
    #[serde(default)]
    pub(super) error: Option<String>,
    #[serde(default)]
    pub(super) message: Option<String>,
    #[serde(default)]
    pub(super) code: Option<String>,
}

// ---------------------------------------------------------------------------
// Discovery DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiVersionResponse {
    pub api_version: String,
    pub supported_versions: Vec<String>,
    pub server_version: String,
    pub status: String,
    #[serde(default)]
    pub capabilities: HashMap<String, bool>,
    #[serde(default)]
    pub endpoints: HashMap<String, String>,
}

impl ApiVersionResponse {
    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }

    pub fn supports_version(&self, version: &str) -> bool {
        self.supported_versions.iter().any(|v| v == version)
    }

    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities.get(cap).copied().unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Authentication DTOs
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
pub struct LoginRequest<'a> {
    pub username: &'a str,
    pub password: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<&'a str>,
}

impl<'a> fmt::Debug for LoginRequest<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginRequest")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("device_name", &self.device_name)
            .field("device_id", &self.device_id)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientUserDto {
    pub id: String,
    pub username: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub refresh_token: String,
    pub user: ClientUserDto,
}

impl fmt::Debug for AuthTokenResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthTokenResponse")
            .field("access_token", &"[REDACTED]")
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .field("refresh_token", &"[REDACTED]")
            .field("user", &self.user)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthRefreshResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub refresh_token: String,
}

impl fmt::Debug for AuthRefreshResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthRefreshResponse")
            .field("access_token", &"[REDACTED]")
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .field("refresh_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Serialize)]
pub struct RefreshTokenRequest<'a> {
    pub refresh_token: &'a str,
}

impl<'a> fmt::Debug for RefreshTokenRequest<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshTokenRequest")
            .field("refresh_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeResponse {
    pub revoked: bool,
}

// ---------------------------------------------------------------------------
// Catalog DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerformerRefDto {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub photo_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogVideoUserStateDto {
    #[serde(default)]
    pub is_favorite: Option<bool>,
    #[serde(default)]
    pub position_seconds: Option<u64>,
    #[serde(default)]
    pub played_percentage: Option<f64>,
    #[serde(default)]
    pub is_played: Option<bool>,
    #[serde(default)]
    pub last_watched_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogVideoItem {
    pub id: String,
    pub title: String,
    pub duration_seconds: u64,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub date_added: Option<String>,
    #[serde(default)]
    pub studio: Option<String>,
    #[serde(default)]
    pub plot: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub performers: Vec<PerformerRefDto>,
    #[serde(default)]
    pub poster_url: Option<String>,
    #[serde(default)]
    pub backdrop_url: Option<String>,
    #[serde(default)]
    pub file_size_bytes: Option<u64>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub is_hd: Option<bool>,
    #[serde(default)]
    pub bitrate_kbps: Option<u32>,
    #[serde(default)]
    pub quality: Option<String>,
    #[serde(default)]
    pub user_state: Option<CatalogVideoUserStateDto>,
}

impl From<CatalogVideoItem> for TvItem {
    fn from(item: CatalogVideoItem) -> Self {
        let year = item
            .release_date
            .as_deref()
            .and_then(|d| d.get(..4))
            .and_then(|y| y.parse::<i32>().ok());

        let is_tg = item.id.starts_with("telegram:");
        let source_provider = if is_tg {
            Some("telegram".to_string())
        } else {
            Some("adult_hub".to_string())
        };
        let provider_video_id = if is_tg { Some(item.id.clone()) } else { None };

        let people = item
            .performers
            .into_iter()
            .map(|p| TvPersonRole {
                id: p.id,
                name: p.name,
                role: Some("Actor".into()),
                person_type: Some("Actor".into()),
                primary_image_tag: p.photo_url,
            })
            .collect();

        let (is_favorite, is_played, playback_position_ticks, played_percentage) =
            if let Some(st) = item.user_state {
                (
                    st.is_favorite.unwrap_or(false),
                    st.is_played.unwrap_or(false),
                    st.position_seconds
                        .map(|s| (s as i64).saturating_mul(10_000_000))
                        .unwrap_or(0),
                    st.played_percentage,
                )
            } else {
                (false, false, 0, None)
            };

        TvItem {
            id: item.id,
            title: item.title,
            source_provider,
            overview: item.plot,
            year,
            runtime_ticks: if item.duration_seconds > 0 {
                Some((item.duration_seconds as i64).saturating_mul(10_000_000))
            } else {
                None
            },
            genres: item.categories,
            tags: item.tags,
            studios: item.studio.into_iter().collect(),
            people,
            primary_image_tag: item.poster_url,
            backdrop_image_tag: item.backdrop_url,
            thumb_image_tag: None,
            child_count: None,
            is_favorite,
            playback_position_ticks,
            played_percentage,
            is_played,
            provider_video_id,
            media_source_id: None,
            container: Some("mp4".into()),
            file_size_bytes: item.file_size_bytes.map(|s| s as i64),
            is_placeholder_source: false,
            width: item.width,
            height: item.height,
            community_rating: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaginatedVideosResponse {
    pub items: Vec<CatalogVideoItem>,
    pub total_count: u64,
    pub offset: u64,
    pub limit: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteStatusResponse {
    #[serde(default)]
    pub video_id: Option<String>,
    pub is_favorite: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FavoritesResponse {
    pub items: Vec<CatalogVideoItem>,
    pub total_count: u64,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub has_more: Option<bool>,
}

impl FavoritesResponse {
    pub fn has_more(&self) -> bool {
        self.has_more.unwrap_or_else(|| {
            let offset = self.offset.unwrap_or(0);
            (offset + self.items.len() as u64) < self.total_count
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct CatalogVideosQuery<'a> {
    pub offset: Option<u64>,
    pub limit: Option<u64>,
    pub sort: Option<&'a str>,
    pub direction: Option<&'a str>,
    pub channel_id: Option<&'a str>,
    pub q: Option<&'a str>,
    pub category: Option<&'a str>,
    pub tag: Option<&'a str>,
    pub studio: Option<&'a str>,
    pub performer: Option<&'a str>,
    pub performer_id: Option<&'a str>,
    pub min_duration: Option<u64>,
    pub is_favorite: Option<bool>,
    pub quality: Option<&'a str>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogChannelItem {
    pub id: String,
    pub title: String,
    pub video_count: u64,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default = "default_true")]
    pub is_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaginatedChannelsResponse {
    pub items: Vec<CatalogChannelItem>,
    pub total_count: u64,
    pub offset: u64,
    pub limit: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CatalogChannelsQuery<'a> {
    pub offset: Option<u64>,
    pub limit: Option<u64>,
    pub sort: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Taxonomy DTOs: Categories, Studios, Tags, Performers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxonomyItemDto {
    pub name: String,
    pub count: u64,
}

impl TaxonomyItemDto {
    pub fn to_category_tv_item(&self) -> TvItem {
        TvItem {
            id: format!("category:{}", self.name),
            title: self.name.clone(),
            genres: vec!["__adult_hub_genre".into()],
            child_count: Some(self.count as u32),
            ..Default::default()
        }
    }

    pub fn to_studio_tv_item(&self) -> TvItem {
        TvItem {
            id: format!("studio:{}", self.name),
            title: self.name.clone(),
            genres: vec!["__adult_hub_studio".into()],
            child_count: Some(self.count as u32),
            ..Default::default()
        }
    }
}

impl From<TaxonomyItemDto> for TvItem {
    fn from(item: TaxonomyItemDto) -> Self {
        item.to_category_tv_item()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxonomyListResponse {
    pub items: Vec<TaxonomyItemDto>,
    pub total_count: u64,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub has_more: Option<bool>,
}

impl TaxonomyListResponse {
    pub fn has_more(&self) -> bool {
        self.has_more.unwrap_or_else(|| {
            let offset = self.offset.unwrap_or(0);
            offset + (self.items.len() as u64) < self.total_count
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct CatalogTaxonomyQuery<'a> {
    pub offset: Option<u64>,
    pub limit: Option<u64>,
    pub q: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerformerItemDto {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub photo_url: Option<String>,
    #[serde(default)]
    pub video_count: Option<u64>,
}

impl From<PerformerItemDto> for TvItem {
    fn from(item: PerformerItemDto) -> Self {
        TvItem {
            id: item.id,
            title: item.name,
            genres: vec!["__adult_hub_performer".into()],
            primary_image_tag: item.photo_url,
            child_count: item.video_count.map(|c| c as u32),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaginatedPerformersResponse {
    pub items: Vec<PerformerItemDto>,
    pub total_count: u64,
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub limit: u64,
    #[serde(default)]
    pub has_more: Option<bool>,
}

impl PaginatedPerformersResponse {
    pub fn has_more(&self) -> bool {
        self.has_more
            .unwrap_or_else(|| self.offset + (self.items.len() as u64) < self.total_count)
    }
}

#[derive(Debug, Clone, Default)]
pub struct CatalogPerformersQuery<'a> {
    pub offset: Option<u64>,
    pub limit: Option<u64>,
    pub q: Option<&'a str>,
    pub letter: Option<&'a str>,
    pub photo_only: Option<bool>,
    pub sort: Option<&'a str>,
    pub direction: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerformerDetailDto {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub disambiguation: Option<String>,
    #[serde(default)]
    pub photo_url: Option<String>,
    #[serde(default)]
    pub video_count: Option<u64>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub nationality: Option<String>,
    #[serde(default)]
    pub birth: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

// ---------------------------------------------------------------------------
// Playback & Direct Play DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackCapabilities {
    pub streaming_protocols: Vec<String>,
    pub supported_containers: Vec<String>,
    #[serde(default)]
    pub seek_alignment_bytes: Option<u64>,
    #[serde(default)]
    pub pinned_representation_required: bool,
    #[serde(default)]
    pub supports_head_requests: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackRenditionDto {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size_bytes: Option<u64>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub stream_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackDescriptorDto {
    pub video_id: String,
    #[serde(default)]
    pub stream_path: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub file_size_bytes: Option<u64>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub renditions: Vec<PlaybackRenditionDto>,
}

impl PlaybackDescriptorDto {
    pub fn default_stream_path(&self) -> Option<&str> {
        if let Some(ref p) = self.stream_path {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
        self.renditions
            .iter()
            .find(|r| r.is_default)
            .and_then(|r| {
                r.stream_path
                    .as_deref()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
            })
            .or_else(|| {
                self.renditions.iter().find_map(|r| {
                    r.stream_path
                        .as_deref()
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                })
            })
    }

    pub fn rendition_by_id(&self, id: &str) -> Option<&PlaybackRenditionDto> {
        self.renditions.iter().find(|r| r.id == id)
    }

    /// Resolves the stream path for a requested rendition ID or defaults to default stream path.
    pub fn rendition_stream_path(&self, rendition_id: Option<&str>) -> Option<&str> {
        if let Some(id) = rendition_id.filter(|s| !s.is_empty() && *s != "original") {
            if let Some(rend) = self.rendition_by_id(id) {
                if let Some(ref p) = rend.stream_path {
                    let trimmed = p.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
            }
        }
        self.default_stream_path()
    }
}

// ---------------------------------------------------------------------------
// Range Resume & Download Types
// ---------------------------------------------------------------------------

/// Metadata persisted alongside partial `.part` download files to prevent
/// appending to an incompatible rendition or source when resuming.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PartFileMetadata {
    pub item_id: String,
    #[serde(default)]
    pub rendition_id: Option<String>,
    pub source_identity: String,
    #[serde(default)]
    pub expected_total_bytes: Option<u64>,
}

impl PartFileMetadata {
    pub fn new(
        item_id: impl Into<String>,
        rendition_id: Option<String>,
        source_identity: impl Into<String>,
        expected_total_bytes: Option<u64>,
    ) -> Self {
        Self {
            item_id: item_id.into(),
            rendition_id,
            source_identity: source_identity.into(),
            expected_total_bytes,
        }
    }

    /// Verifies if existing partial metadata matches the requested download parameters.
    pub fn matches(
        &self,
        item_id: &str,
        rendition_id: Option<&str>,
        source_identity: &str,
    ) -> bool {
        self.item_id == item_id
            && self.rendition_id.as_deref() == rendition_id
            && self.source_identity == source_identity
    }

    /// Reads metadata sidecar file for a given part path, returning None if absent or corrupt.
    pub fn read_from_sidecar(part_path: &std::path::Path) -> Option<Self> {
        let meta_path = Self::sidecar_path(part_path);
        let bytes = std::fs::read(meta_path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Saves metadata to sidecar file for a given part path.
    pub fn save_to_sidecar(&self, part_path: &std::path::Path) -> std::io::Result<()> {
        let meta_path = Self::sidecar_path(part_path);
        let serialized = serde_json::to_vec(self)?;
        std::fs::write(meta_path, serialized)
    }

    /// Removes metadata sidecar file for a given part path if present.
    pub fn remove_sidecar(part_path: &std::path::Path) {
        let meta_path = Self::sidecar_path(part_path);
        let _ = std::fs::remove_file(meta_path);
    }

    pub fn sidecar_path(part_path: &std::path::Path) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("{}.meta", part_path.to_string_lossy()))
    }
}

/// Decision outcome for HTTP Range resume compatibility check
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeResumeOutcome {
    /// Resume by appending at start_byte (typically existing_bytes)
    Resume {
        start_byte: u64,
        total_bytes: Option<u64>,
    },
    /// Incompatible range offset, size mismatch, or full-file response (200 OK): restart safely from byte 0
    Restart {
        reason: String,
        total_bytes: Option<u64>,
    },
    /// Fatal or non-resumable status error
    Error { status: u16, message: String },
}

/// Evaluates server response against partial download state according to RFC 7233.
///
/// Ensures range/206 resume only proceeds when the selected source/encoding identity
/// remains compatible; otherwise safely signals restart or surfaces a recoverable error.
pub fn evaluate_range_resume(
    status_code: u16,
    content_range_header: Option<&str>,
    content_length: Option<u64>,
    existing_bytes: u64,
    previous_total: Option<u64>,
) -> RangeResumeOutcome {
    if status_code == 206 {
        let Some(cr) = content_range_header
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return RangeResumeOutcome::Restart {
                reason: "HTTP 206 missing Content-Range header".to_string(),
                total_bytes: content_length.map(|l| existing_bytes + l),
            };
        };

        // Format: bytes <start>-<end>/<total>
        let Some(range_spec) = cr.strip_prefix("bytes ") else {
            return RangeResumeOutcome::Restart {
                reason: format!("Unrecognized Content-Range unit in '{cr}'"),
                total_bytes: None,
            };
        };

        let mut parts = range_spec.split('/');
        let range_part = parts.next().unwrap_or("");
        let total_part = parts.next();

        let server_start: Option<u64> = range_part
            .split_once('-')
            .and_then(|(s, _)| s.trim().parse::<u64>().ok());

        let server_total: Option<u64> = total_part
            .map(str::trim)
            .filter(|t| *t != "*")
            .and_then(|t| t.parse::<u64>().ok());

        // Validate start offset matches existing_bytes
        match server_start {
            Some(start) if start == existing_bytes => {}
            Some(start) => {
                return RangeResumeOutcome::Restart {
                    reason: format!(
                        "Content-Range start offset {start} does not match local existing bytes {existing_bytes}"
                    ),
                    total_bytes: server_total,
                };
            }
            None => {
                return RangeResumeOutcome::Restart {
                    reason: format!("Failed to parse start offset from Content-Range '{cr}'"),
                    total_bytes: server_total,
                };
            }
        }

        // Validate total size compatibility if previously known
        if let (Some(srv_tot), Some(prev_tot)) = (server_total, previous_total) {
            if prev_tot > 0 && srv_tot != prev_tot {
                return RangeResumeOutcome::Restart {
                    reason: format!(
                        "Underlying stream file size changed from {prev_tot} to {srv_tot} bytes; source or encoding identity changed"
                    ),
                    total_bytes: Some(srv_tot),
                };
            }
        }

        RangeResumeOutcome::Resume {
            start_byte: existing_bytes,
            total_bytes: server_total,
        }
    } else if status_code == 200 {
        if existing_bytes > 0 {
            RangeResumeOutcome::Restart {
                reason: "Server returned full file (HTTP 200 OK) ignoring Range request"
                    .to_string(),
                total_bytes: content_length,
            }
        } else {
            RangeResumeOutcome::Resume {
                start_byte: 0,
                total_bytes: content_length,
            }
        }
    } else if status_code == 416 {
        if existing_bytes > 0 {
            RangeResumeOutcome::Restart {
                reason: "Range not satisfiable (HTTP 416); local partial bytes out of bounds"
                    .to_string(),
                total_bytes: None,
            }
        } else {
            RangeResumeOutcome::Error {
                status: 416,
                message: "Range not satisfiable (HTTP 416)".to_string(),
            }
        }
    } else if status_code == 401 {
        RangeResumeOutcome::Error {
            status: 401,
            message: "Session expired (401 Unauthorized). Please sign in again.".to_string(),
        }
    } else {
        RangeResumeOutcome::Error {
            status: status_code,
            message: format!("Server returned HTTP {status_code}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Watch Progress DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchProgressDto {
    pub video_id: String,
    pub position_seconds: u64,
    pub duration_seconds: u64,
    pub played_percentage: f64,
    pub is_played: bool,
    pub last_watched_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchProgressUpsertRequest {
    pub position_seconds: u64,
    pub duration_seconds: u64,
    pub is_played: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub played_percentage: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_watched_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchProgressUpsertResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchProgressDeleteResponse {
    pub deleted: bool,
}
