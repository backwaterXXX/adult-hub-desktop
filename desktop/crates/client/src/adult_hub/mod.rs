use crate::policy::{ServiceUrl, ServiceUrlPolicy, ServiceUrlValidation};
use crate::urlencoding;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, RANGE};
use serde::Deserialize;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use thiserror::Error;
use url::Url;

// ---------------------------------------------------------------------------
// Error Types
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum AdultHubError {
    #[error("invalid service URL: {0}")]
    InvalidUrl(String),

    #[error("origin mismatch: expected {expected}, got {actual}")]
    OriginMismatch { expected: String, actual: String },

    #[error("unauthorized (HTTP {status}): {message}")]
    Unauthorized { status: u16, message: String },

    #[error("not found (HTTP 404): {0}")]
    NotFound(String),

    #[error("rate limited (HTTP 429): {0}")]
    RateLimited(String),

    #[error("conflict (HTTP 409): {0}")]
    Conflict(String),

    #[error("payload too large: response exceeded {limit} bytes")]
    PayloadTooLarge { limit: usize },

    #[error("api error (HTTP {status}): {message}")]
    Api {
        status: u16,
        message: String,
        code: Option<String>,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl AdultHubError {
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, AdultHubError::Unauthorized { .. })
    }

    pub fn is_rate_limited(&self) -> bool {
        matches!(self, AdultHubError::RateLimited(_))
    }
}

pub type Result<T> = std::result::Result<T, AdultHubError>;

mod api_types;
pub use api_types::*;

#[derive(Clone)]
pub struct AdultHubClient {
    client: Client,
    stream_client: Client,
    service_url: ServiceUrl,
    parsed_base_url: Url,
    token: Arc<RwLock<Option<String>>>,
    device_name: Option<String>,
    device_id: Option<String>,
}

impl fmt::Debug for AdultHubClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let has_token = self.token.read().map(|t| t.is_some()).unwrap_or(false);
        f.debug_struct("AdultHubClient")
            .field("base_url", &self.service_url.normalized)
            .field("origin", &self.service_url.origin)
            .field("device_name", &self.device_name)
            .field("device_id", &self.device_id)
            .field("has_token", &has_token)
            .finish()
    }
}

impl AdultHubClient {
    /// Creates a new `AdultHubClient` targeting the given server base URL.
    ///
    /// Validates the URL according to `ServiceUrlPolicy` (rejecting public cleartext HTTP,
    /// credentials, fragments, queries) and initializes clients with redirects disabled
    /// to prevent credential or token forwarding across origins.
    ///
    /// Distinguishes between ordinary API calls (bounded connect and request timeouts)
    /// and long-running streaming requests (bounded connect timeout, but no overall request timeout
    /// so full media playback sessions and seeking are never terminated after 30 seconds).
    pub fn new(base_url: &str) -> Result<Self> {
        let service_url = match ServiceUrlPolicy::inspect(base_url, false) {
            ServiceUrlValidation::Valid(v) => v,
            ServiceUrlValidation::Invalid(err) => return Err(AdultHubError::InvalidUrl(err)),
        };

        let parsed_base_url = Url::parse(&service_url.normalized)
            .map_err(|e| AdultHubError::InvalidUrl(e.to_string()))?;

        // Client for ordinary API calls (discovery, login, catalog) with safe timeouts.
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(AdultHubError::Http)?;

        // Dedicated client for media streaming. Bounded connect_timeout (10s) and redirects disabled,
        // but NO total timeout so that long media playback sessions and seeking can continue
        // indefinitely without being terminated after 30 seconds.
        let stream_client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(AdultHubError::Http)?;

        Ok(Self {
            client,
            stream_client,
            service_url,
            parsed_base_url,
            token: Arc::new(RwLock::new(None)),
            device_name: None,
            device_id: None,
        })
    }

    /// Returns a reference to the HTTP client used for ordinary API operations (with bounded timeouts).
    pub fn api_client(&self) -> &Client {
        &self.client
    }

    /// Returns a reference to the HTTP client used for long-running streaming operations (no total timeout).
    pub fn stream_client(&self) -> &Client {
        &self.stream_client
    }

    /// Discovers server capabilities without requiring an authenticated client instance.
    pub fn discover(base_url: &str) -> Result<ApiVersionResponse> {
        let client = Self::new(base_url)?;
        client.discover_version()
    }

    /// Configures default device credentials for authentication flows.
    pub fn with_device_info(
        mut self,
        device_name: Option<String>,
        device_id: Option<String>,
    ) -> Self {
        self.device_name = device_name;
        self.device_id = device_id;
        self
    }

    /// Configures an initial Bearer access token.
    pub fn with_token(self, token: impl Into<String>) -> Self {
        let token_val = token.into();
        if let Ok(mut guard) = self.token.write() {
            *guard = if token_val.trim().is_empty() {
                None
            } else {
                Some(token_val.trim().to_string())
            };
        }
        self
    }

    /// Sets or clears the active Bearer access token.
    pub fn set_token(&self, token: Option<String>) {
        if let Ok(mut guard) = self.token.write() {
            *guard = token.and_then(|t| {
                let trimmed = t.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            });
        }
    }

    /// Returns the currently configured Bearer access token if present.
    pub fn token(&self) -> Option<String> {
        self.token.read().ok().and_then(|t| t.clone())
    }

    /// The normalized base URL (without trailing slash).
    pub fn base_url(&self) -> &str {
        &self.service_url.normalized
    }

    /// The canonical origin (scheme://host:port).
    pub fn origin(&self) -> &str {
        &self.service_url.origin
    }

    /// Constructs a slash-safe URL strictly bounded to the configured single origin.
    pub fn build_url(&self, endpoint: &str) -> Result<Url> {
        let clean_base = self.service_url.normalized.trim_end_matches('/');
        let clean_endpoint = endpoint.trim_start_matches('/');
        if clean_endpoint.split('/').any(|s| s == "." || s == "..") {
            return Err(AdultHubError::InvalidUrl(
                "dot segments are forbidden in endpoints".into(),
            ));
        }
        let raw = if clean_endpoint.is_empty() {
            clean_base.to_string()
        } else {
            format!("{clean_base}/{clean_endpoint}")
        };

        let parsed = Url::parse(&raw).map_err(|e| AdultHubError::InvalidUrl(e.to_string()))?;

        // Strict single-origin check: scheme, host, and port must match base URL
        if parsed.scheme() != self.parsed_base_url.scheme()
            || parsed.host_str() != self.parsed_base_url.host_str()
            || parsed.port_or_known_default() != self.parsed_base_url.port_or_known_default()
        {
            return Err(AdultHubError::OriginMismatch {
                expected: self.service_url.origin.clone(),
                actual: parsed.origin().ascii_serialization(),
            });
        }

        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(AdultHubError::InvalidUrl(
                "embedded credentials / userinfo are not permitted".into(),
            ));
        }

        Ok(parsed)
    }

    /// Resolves artwork or media paths against the client's origin with single-origin enforcement.
    pub fn resolve_artwork_url(&self, path_or_url: &str) -> Result<String> {
        let trimmed = path_or_url.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            let parsed =
                Url::parse(trimmed).map_err(|e| AdultHubError::InvalidUrl(e.to_string()))?;
            if parsed.scheme() != self.parsed_base_url.scheme()
                || parsed.host_str() != self.parsed_base_url.host_str()
                || parsed.port_or_known_default() != self.parsed_base_url.port_or_known_default()
            {
                return Err(AdultHubError::OriginMismatch {
                    expected: self.service_url.origin.clone(),
                    actual: parsed.origin().ascii_serialization(),
                });
            }
            if !parsed.username().is_empty() || parsed.password().is_some() {
                return Err(AdultHubError::InvalidUrl(
                    "embedded credentials / userinfo are not permitted".into(),
                ));
            }
            return Ok(parsed.to_string());
        }

        let url = self.build_url(trimmed)?;
        Ok(url.to_string())
    }

    /// Return the resolved artwork URL for a video and kind (poster, backdrop, thumb, primary).
    pub fn get_video_artwork_url(&self, video_id: &str, kind: &str) -> Result<String> {
        let clean_kind = kind.trim();
        if !matches!(clean_kind, "poster" | "backdrop" | "thumb" | "primary") {
            return Err(AdultHubError::InvalidUrl(format!(
                "unsupported artwork kind: {kind}"
            )));
        }
        let enc_id = urlencoding(video_id);
        let path = format!("/api/v1/artwork/video/{enc_id}/{clean_kind}");
        self.resolve_artwork_url(&path)
    }

    /// Return the resolved artwork URL for a performer.
    pub fn get_performer_artwork_url(&self, performer_id: &str) -> Result<String> {
        let enc_id = urlencoding(performer_id);
        let path = format!("/api/v1/artwork/performer/{enc_id}");
        self.resolve_artwork_url(&path)
    }

    /// Return the resolved artwork URL for a channel.
    pub fn get_channel_artwork_url(&self, channel_id: &str) -> Result<String> {
        let enc_id = urlencoding(channel_id);
        let path = format!("/api/v1/artwork/channel/{enc_id}");
        self.resolve_artwork_url(&path)
    }

    /// Fetch binary image bytes for artwork, strictly authenticating with bearer token.
    pub fn get_artwork_bytes(&self, path_or_url: &str) -> Result<Vec<u8>> {
        let url = self.resolve_artwork_url(path_or_url)?;
        let mut req = self.client.get(&url);
        req = self.attach_auth(req);
        let mut res = req.send().map_err(AdultHubError::Http)?;
        let status = res.status();
        let status_code = status.as_u16();
        if status.is_success() {
            const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
            if let Some(cl) = res.content_length() {
                if cl > MAX_IMAGE_BYTES as u64 {
                    return Err(AdultHubError::PayloadTooLarge {
                        limit: MAX_IMAGE_BYTES,
                    });
                }
            }
            use std::io::Read;
            let mut buffer = Vec::new();
            let mut limited = (&mut res).take((MAX_IMAGE_BYTES + 1) as u64);
            limited
                .read_to_end(&mut buffer)
                .map_err(AdultHubError::Io)?;
            if buffer.len() > MAX_IMAGE_BYTES {
                return Err(AdultHubError::PayloadTooLarge {
                    limit: MAX_IMAGE_BYTES,
                });
            }
            return Ok(buffer);
        }
        const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
        use std::io::Read;
        let mut err_buf = Vec::new();
        let mut limited_err = (&mut res).take((MAX_ERROR_BODY_BYTES + 1) as u64);
        limited_err
            .read_to_end(&mut err_buf)
            .map_err(AdultHubError::Io)?;
        let err_text = String::from_utf8_lossy(&err_buf[..err_buf.len().min(MAX_ERROR_BODY_BYTES)]);
        let detail = if let Ok(prob) = serde_json::from_str::<ProblemDetails>(&err_text) {
            prob.detail
                .or(prob.error)
                .or(prob.message)
                .or(prob.title)
                .unwrap_or_else(|| format!("HTTP {status_code}"))
        } else if !err_text.trim().is_empty() {
            err_text.chars().take(250).collect()
        } else {
            format!("HTTP {status_code}")
        };
        match status_code {
            401 | 403 => Err(AdultHubError::Unauthorized {
                status: status_code,
                message: detail,
            }),
            404 => Err(AdultHubError::NotFound(detail)),
            429 => Err(AdultHubError::RateLimited(detail)),
            _ => Err(AdultHubError::Api {
                status: status_code,
                message: detail,
                code: None,
            }),
        }
    }

    fn attach_auth(&self, builder: RequestBuilder) -> RequestBuilder {
        if let Ok(guard) = self.token.read() {
            if let Some(token) = guard.as_deref() {
                if !token.is_empty() {
                    return builder.header(AUTHORIZATION, format!("Bearer {token}"));
                }
            }
        }
        builder
    }

    fn handle_response<T: for<'de> Deserialize<'de>>(&self, res: Response) -> Result<T> {
        let status = res.status();
        let status_code = status.as_u16();

        if status.is_success() {
            let bytes = res.bytes().map_err(AdultHubError::Http)?;
            let body = serde_json::from_slice::<T>(&bytes).map_err(AdultHubError::Serialization)?;
            return Ok(body);
        }

        let err_text = res.text().unwrap_or_default();
        let mut code_out = None;
        let detail = if let Ok(prob) = serde_json::from_str::<ProblemDetails>(&err_text) {
            code_out = prob.code;
            prob.detail
                .or(prob.error)
                .or(prob.message)
                .or(prob.title)
                .unwrap_or_else(|| format!("HTTP {status_code}"))
        } else if !err_text.trim().is_empty() {
            err_text.chars().take(250).collect()
        } else {
            format!("HTTP {status_code}")
        };

        match status_code {
            401 | 403 => Err(AdultHubError::Unauthorized {
                status: status_code,
                message: detail,
            }),
            404 => Err(AdultHubError::NotFound(detail)),
            409 => Err(AdultHubError::Conflict(detail)),
            429 => Err(AdultHubError::RateLimited(detail)),
            _ => Err(AdultHubError::Api {
                status: status_code,
                message: detail,
                code: code_out,
            }),
        }
    }

    // -----------------------------------------------------------------------
    // Discovery
    // -----------------------------------------------------------------------

    /// Queries `GET /api/v1/version` for API version, capabilities, and service status.
    pub fn discover_version(&self) -> Result<ApiVersionResponse> {
        let url = self.build_url("/api/v1/version")?;
        let res = self.client.get(url).send()?;
        self.handle_response(res)
    }

    // -----------------------------------------------------------------------
    // Authentication
    // -----------------------------------------------------------------------

    /// Authenticates with username and password, storing the issued Bearer access token.
    pub fn login(&self, username: &str, password: &str) -> Result<AuthTokenResponse> {
        self.login_with_device(
            username,
            password,
            self.device_name.as_deref(),
            self.device_id.as_deref(),
        )
    }

    /// Authenticates with explicit device info, storing the issued access token on success.
    pub fn login_with_device(
        &self,
        username: &str,
        password: &str,
        device_name: Option<&str>,
        device_id: Option<&str>,
    ) -> Result<AuthTokenResponse> {
        let url = self.build_url("/api/v1/auth/login")?;
        let payload = LoginRequest {
            username,
            password,
            device_name,
            device_id,
        };

        let res = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()?;

        let token_resp: AuthTokenResponse = self.handle_response(res)?;
        self.set_token(Some(token_resp.access_token.clone()));
        Ok(token_resp)
    }

    /// Rotates a single-use refresh token, obtaining a fresh access token and refresh token.
    pub fn refresh_token(&self, refresh_token: &str) -> Result<AuthRefreshResponse> {
        let url = self.build_url("/api/v1/auth/refresh")?;
        let payload = RefreshTokenRequest { refresh_token };

        let res = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()?;

        let token_resp: AuthRefreshResponse = self.handle_response(res)?;
        self.set_token(Some(token_resp.access_token.clone()));
        Ok(token_resp)
    }

    /// Explicitly revokes a refresh token family.
    pub fn revoke_token(&self, refresh_token: &str) -> Result<RevokeResponse> {
        let url = self.build_url("/api/v1/auth/revoke")?;
        let payload = RefreshTokenRequest { refresh_token };

        let res = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()?;

        let revoke_resp: RevokeResponse = self.handle_response(res)?;
        if revoke_resp.revoked {
            self.set_token(None);
        }
        Ok(revoke_resp)
    }

    // -----------------------------------------------------------------------
    // Catalog Browse & Detail
    // -----------------------------------------------------------------------

    /// Queries `GET /api/v1/catalog/videos` with pagination, sort, and search filters.
    pub fn get_catalog_videos(
        &self,
        query: &CatalogVideosQuery,
    ) -> Result<PaginatedVideosResponse> {
        let mut url = self.build_url("/api/v1/catalog/videos")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(off) = query.offset {
                pairs.append_pair("offset", &off.to_string());
            }
            if let Some(lim) = query.limit {
                pairs.append_pair("limit", &lim.to_string());
            }
            if let Some(sort) = query.sort {
                pairs.append_pair("sort", sort);
            }
            if let Some(direction) = query.direction {
                pairs.append_pair("direction", direction);
            }
            if let Some(ch) = query.channel_id {
                pairs.append_pair("channel_id", ch);
            }
            if let Some(q) = query.q {
                pairs.append_pair("q", q);
            }
            if let Some(cat) = query.category {
                pairs.append_pair("category", cat);
            }
            if let Some(tag) = query.tag {
                pairs.append_pair("tag", tag);
            }
            if let Some(stu) = query.studio {
                pairs.append_pair("studio", stu);
            }
            if let Some(perf) = query.performer {
                pairs.append_pair("performer", perf);
            }
            if let Some(pid) = query.performer_id {
                pairs.append_pair("performer_id", pid);
            }
            if let Some(min_dur) = query.min_duration {
                pairs.append_pair("min_duration", &min_dur.to_string());
            }
            if let Some(is_fav) = query.is_favorite {
                pairs.append_pair("is_favorite", &is_fav.to_string());
            }
        }

        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    // -----------------------------------------------------------------------
    // Taxonomy Browsing & Detail
    // -----------------------------------------------------------------------

    /// Queries `GET /api/v1/catalog/categories` with pagination and optional search filter.
    pub fn get_categories(&self, query: &CatalogTaxonomyQuery) -> Result<TaxonomyListResponse> {
        let mut url = self.build_url("/api/v1/catalog/categories")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(off) = query.offset {
                pairs.append_pair("offset", &off.to_string());
            }
            if let Some(lim) = query.limit {
                pairs.append_pair("limit", &lim.to_string());
            }
            if let Some(q) = query.q {
                pairs.append_pair("q", q);
            }
        }

        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Queries `GET /api/v1/catalog/studios` with pagination and optional search filter.
    pub fn get_studios(&self, query: &CatalogTaxonomyQuery) -> Result<TaxonomyListResponse> {
        let mut url = self.build_url("/api/v1/catalog/studios")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(off) = query.offset {
                pairs.append_pair("offset", &off.to_string());
            }
            if let Some(lim) = query.limit {
                pairs.append_pair("limit", &lim.to_string());
            }
            if let Some(q) = query.q {
                pairs.append_pair("q", q);
            }
        }

        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Queries `GET /api/v1/catalog/tags` with pagination and optional search filter.
    pub fn get_tags(&self, query: &CatalogTaxonomyQuery) -> Result<TaxonomyListResponse> {
        let mut url = self.build_url("/api/v1/catalog/tags")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(off) = query.offset {
                pairs.append_pair("offset", &off.to_string());
            }
            if let Some(lim) = query.limit {
                pairs.append_pair("limit", &lim.to_string());
            }
            if let Some(q) = query.q {
                pairs.append_pair("q", q);
            }
        }

        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Queries `GET /api/v1/catalog/performers` with pagination, filters, and sort.
    pub fn get_performers(
        &self,
        query: &CatalogPerformersQuery,
    ) -> Result<PaginatedPerformersResponse> {
        let mut url = self.build_url("/api/v1/catalog/performers")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(off) = query.offset {
                pairs.append_pair("offset", &off.to_string());
            }
            if let Some(lim) = query.limit {
                pairs.append_pair("limit", &lim.to_string());
            }
            if let Some(q) = query.q {
                pairs.append_pair("q", q);
            }
            if let Some(letter) = query.letter {
                pairs.append_pair("letter", letter);
            }
            if let Some(photo_only) = query.photo_only {
                pairs.append_pair("photo_only", &photo_only.to_string());
            }
            if let Some(sort) = query.sort {
                pairs.append_pair("sort", sort);
            }
            if let Some(direction) = query.direction {
                pairs.append_pair("direction", direction);
            }
        }

        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Queries `GET /api/v1/catalog/performers/{performer_id}` for performer biographical details.
    pub fn get_performer_detail(&self, performer_id: &str) -> Result<PerformerDetailDto> {
        let clean_id = performer_id.trim();
        if clean_id.is_empty() {
            return Err(AdultHubError::InvalidUrl(
                "performer_id cannot be empty".into(),
            ));
        }

        let encoded_id = urlencoding(clean_id);
        let endpoint = format!("/api/v1/catalog/performers/{encoded_id}");
        let url = self.build_url(&endpoint)?;

        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Alias for `get_performer_detail`.
    pub fn get_performer(&self, performer_id: &str) -> Result<PerformerDetailDto> {
        self.get_performer_detail(performer_id)
    }

    /// Queries `GET /api/v1/catalog/videos` with pagination, sort, and search filters.
    pub fn get_videos(&self, query: &CatalogVideosQuery) -> Result<PaginatedVideosResponse> {
        self.get_catalog_videos(query)
    }

    /// Queries `GET /api/v1/catalog/videos/{video_id}` for video detail.
    pub fn get_catalog_video_detail(&self, video_id: &str) -> Result<CatalogVideoItem> {
        let clean_id = video_id.trim();
        if clean_id.is_empty() {
            return Err(AdultHubError::InvalidUrl("video_id cannot be empty".into()));
        }

        let encoded_id = urlencoding(clean_id);
        let endpoint = format!("/api/v1/catalog/videos/{encoded_id}");
        let url = self.build_url(&endpoint)?;

        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    // -----------------------------------------------------------------------
    // Catalog Channels
    // -----------------------------------------------------------------------

    /// Queries `GET /api/v1/catalog/channels` for registered Telegram source channels.
    pub fn get_catalog_channels(
        &self,
        query: &CatalogChannelsQuery,
    ) -> Result<PaginatedChannelsResponse> {
        let mut url = self.build_url("/api/v1/catalog/channels")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(off) = query.offset {
                pairs.append_pair("offset", &off.to_string());
            }
            if let Some(lim) = query.limit {
                pairs.append_pair("limit", &lim.to_string());
            }
            if let Some(sort) = query.sort {
                pairs.append_pair("sort", sort);
            }
        }

        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Queries `GET /api/v1/catalog/channels/{channel_id}` for channel detail and video counts.
    pub fn get_catalog_channel_detail(&self, channel_id: &str) -> Result<CatalogChannelItem> {
        let clean_id = channel_id.trim();
        if clean_id.is_empty() {
            return Err(AdultHubError::InvalidUrl(
                "channel_id cannot be empty".into(),
            ));
        }

        let encoded_id = urlencoding(clean_id);
        let endpoint = format!("/api/v1/catalog/channels/{encoded_id}");
        let url = self.build_url(&endpoint)?;

        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Returns the current bearer authorization header value (e.g. `Bearer <token>`) if set.
    pub fn auth_header_value(&self) -> Option<String> {
        if let Ok(guard) = self.token.read() {
            if let Some(token) = guard.as_deref() {
                if !token.is_empty() {
                    return Some(format!("Bearer {token}"));
                }
            }
        }
        None
    }

    /// Queries `GET /api/v1/playback/capabilities` for playback engine streaming capabilities.
    pub fn get_playback_capabilities(&self) -> Result<PlaybackCapabilities> {
        let url = self.build_url("/api/v1/playback/capabilities")?;
        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Queries `GET /api/v1/playback/renditions/{video_id}` for playback descriptor and safe rendition choices.
    pub fn get_playback_descriptor(&self, video_id: &str) -> Result<PlaybackDescriptorDto> {
        let clean_id = video_id.trim();
        if clean_id.is_empty() {
            return Err(AdultHubError::InvalidUrl("video_id cannot be empty".into()));
        }
        let encoded_id = urlencoding(clean_id);
        let endpoint = format!("/api/v1/playback/renditions/{encoded_id}");
        let url = self.build_url(&endpoint)?;
        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Resolves and validates a stream path or absolute stream URL strictly enforcing single-origin policy.
    ///
    /// Rejects empty or whitespace paths with `AdultHubError::InvalidUrl`.
    /// Rejects cross-origin absolute stream URLs with `AdultHubError::OriginMismatch`.
    /// Same-origin relative paths (`/api/v1/playback/stream/...`) or same-origin absolute URLs
    /// resolve to a valid normalized string URL.
    pub fn resolve_stream_url(&self, stream_path_or_url: &str) -> Result<String> {
        let trimmed = stream_path_or_url.trim();
        if trimmed.is_empty() {
            return Err(AdultHubError::InvalidUrl(
                "stream path cannot be empty".into(),
            ));
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            let parsed =
                Url::parse(trimmed).map_err(|e| AdultHubError::InvalidUrl(e.to_string()))?;
            if parsed.scheme() != self.parsed_base_url.scheme()
                || parsed.host_str() != self.parsed_base_url.host_str()
                || parsed.port_or_known_default() != self.parsed_base_url.port_or_known_default()
            {
                return Err(AdultHubError::OriginMismatch {
                    expected: self.service_url.origin.clone(),
                    actual: parsed.origin().ascii_serialization(),
                });
            }
            if !parsed.username().is_empty() || parsed.password().is_some() {
                return Err(AdultHubError::InvalidUrl(
                    "embedded credentials / userinfo are not permitted".into(),
                ));
            }
            return Ok(parsed.to_string());
        }

        let url = self.build_url(trimmed)?;
        Ok(url.to_string())
    }

    /// Builds a GET streaming request for a validated same-origin stream path or URL.
    ///
    /// Uses the dedicated `stream_client` (unbounded stream duration, bounded connect timeout).
    /// Strictly rejects cross-origin URLs and attaches bearer authorization and optional range headers.
    pub fn build_stream_request(
        &self,
        stream_path_or_url: &str,
        range_header: Option<&str>,
    ) -> Result<RequestBuilder> {
        let validated_url = self.resolve_stream_url(stream_path_or_url)?;
        let mut builder = self.stream_client.get(&validated_url);
        builder = self.attach_auth(builder);
        if let Some(range) = range_header {
            let trimmed = range.trim();
            if !trimmed.is_empty() {
                builder = builder.header(RANGE, trimmed);
            }
        }
        Ok(builder)
    }

    /// Builds a HEAD streaming request for a validated same-origin stream path or URL.
    ///
    /// Uses the dedicated `stream_client` (unbounded stream duration, bounded connect timeout).
    /// Strictly rejects cross-origin URLs and attaches bearer authorization and optional range headers.
    pub fn build_head_stream_request(
        &self,
        stream_path_or_url: &str,
        range_header: Option<&str>,
    ) -> Result<RequestBuilder> {
        let validated_url = self.resolve_stream_url(stream_path_or_url)?;
        let mut builder = self.stream_client.head(&validated_url);
        builder = self.attach_auth(builder);
        if let Some(range) = range_header {
            let trimmed = range.trim();
            if !trimmed.is_empty() {
                builder = builder.header(RANGE, trimmed);
            }
        }
        Ok(builder)
    }

    /// Fetches authenticated owner-scoped watch progress for a video.
    ///
    /// Resolves canonical duplicate aliases and returns `Ok(Some(dto))` if recorded,
    /// or `Ok(None)` if no progress exists (HTTP 404).
    pub fn get_watch_progress(&self, video_id: &str) -> Result<Option<WatchProgressDto>> {
        let clean_id = video_id.trim();
        if clean_id.is_empty() {
            return Err(AdultHubError::InvalidUrl("video_id cannot be empty".into()));
        }

        let encoded_id = urlencoding(clean_id);
        let endpoint = format!("/api/v1/user/watch/{encoded_id}");
        let url = self.build_url(&endpoint)?;

        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        let status = res.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let dto: WatchProgressDto = self.handle_response(res)?;
        Ok(Some(dto))
    }

    /// Alias for `get_watch_progress`.
    pub fn get_progress(&self, video_id: &str) -> Result<Option<WatchProgressDto>> {
        self.get_watch_progress(video_id)
    }

    /// Upserts authenticated owner-scoped watch progress for a video.
    ///
    /// Revalidates input bounds and resolves canonical keeper ID on duplicate aliases before upsert.
    pub fn put_watch_progress(
        &self,
        video_id: &str,
        request: &WatchProgressUpsertRequest,
    ) -> Result<WatchProgressUpsertResponse> {
        let clean_id = video_id.trim();
        if clean_id.is_empty() {
            return Err(AdultHubError::InvalidUrl("video_id cannot be empty".into()));
        }

        let encoded_id = urlencoding(clean_id);
        let endpoint = format!("/api/v1/user/watch/{encoded_id}");
        let url = self.build_url(&endpoint)?;

        let req = self.attach_auth(self.client.put(url).json(request));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Alias for `put_watch_progress`.
    pub fn put_progress(
        &self,
        video_id: &str,
        request: &WatchProgressUpsertRequest,
    ) -> Result<WatchProgressUpsertResponse> {
        self.put_watch_progress(video_id, request)
    }

    /// Deletes watch progress for a video and any active duplicate aliases.
    pub fn delete_watch_progress(&self, video_id: &str) -> Result<bool> {
        let clean_id = video_id.trim();
        if clean_id.is_empty() {
            return Err(AdultHubError::InvalidUrl("video_id cannot be empty".into()));
        }

        let encoded_id = urlencoding(clean_id);
        let endpoint = format!("/api/v1/user/watch/{encoded_id}");
        let url = self.build_url(&endpoint)?;

        let req = self.attach_auth(self.client.delete(url));
        let res = req.send()?;
        let resp: WatchProgressDeleteResponse = self.handle_response(res)?;
        Ok(resp.deleted)
    }

    /// Checks if a video is favorited by the authenticated user (`GET /api/v1/user/favorites/{video_id}`).
    pub fn get_favorite_status(&self, video_id: &str) -> Result<FavoriteStatusResponse> {
        let clean_id = video_id.trim();
        if clean_id.is_empty() {
            return Err(AdultHubError::InvalidUrl("video_id cannot be empty".into()));
        }

        let encoded_id = urlencoding(clean_id);
        let endpoint = format!("/api/v1/user/favorites/{encoded_id}");
        let url = self.build_url(&endpoint)?;

        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Adds a video to authenticated user favorites (`PUT /api/v1/user/favorites/{video_id}`).
    pub fn add_favorite(&self, video_id: &str) -> Result<FavoriteStatusResponse> {
        let clean_id = video_id.trim();
        if clean_id.is_empty() {
            return Err(AdultHubError::InvalidUrl("video_id cannot be empty".into()));
        }

        let encoded_id = urlencoding(clean_id);
        let endpoint = format!("/api/v1/user/favorites/{encoded_id}");
        let url = self.build_url(&endpoint)?;

        let req = self.attach_auth(self.client.put(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Removes a video and active duplicate aliases from authenticated user favorites (`DELETE /api/v1/user/favorites/{video_id}`).
    pub fn remove_favorite(&self, video_id: &str) -> Result<FavoriteStatusResponse> {
        let clean_id = video_id.trim();
        if clean_id.is_empty() {
            return Err(AdultHubError::InvalidUrl("video_id cannot be empty".into()));
        }

        let encoded_id = urlencoding(clean_id);
        let endpoint = format!("/api/v1/user/favorites/{encoded_id}");
        let url = self.build_url(&endpoint)?;

        let req = self.attach_auth(self.client.delete(url));
        let res = req.send()?;
        self.handle_response(res)
    }

    /// Lists authenticated user favorites with deterministic pagination (`GET /api/v1/user/favorites`).
    pub fn get_favorites(
        &self,
        offset: Option<u64>,
        limit: Option<u64>,
        sort: Option<&str>,
        direction: Option<&str>,
    ) -> Result<FavoritesResponse> {
        let mut url = self.build_url("/api/v1/user/favorites")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(off) = offset {
                pairs.append_pair("offset", &off.to_string());
            }
            if let Some(lim) = limit {
                pairs.append_pair("limit", &lim.to_string());
            }
            if let Some(s) = sort {
                pairs.append_pair("sort", s);
            }
            if let Some(d) = direction {
                pairs.append_pair("direction", d);
            }
        }

        let req = self.attach_auth(self.client.get(url));
        let res = req.send()?;
        self.handle_response(res)
    }
}

// ---------------------------------------------------------------------------
// Concurrency & In-Flight Request Deduplication Guard
// ---------------------------------------------------------------------------

/// Thread-safe in-flight deduplication guard for asynchronous operations keyed by item ID.
///
/// Prevents concurrent out-of-order requests or double-click races for the same resource.
#[derive(Debug, Default, Clone)]
pub struct InFlightGuard {
    in_flight: Arc<RwLock<std::collections::HashSet<String>>>,
}

/// Active lease on an in-flight key that automatically releases on drop.
pub struct InFlightSlot {
    in_flight: Arc<RwLock<std::collections::HashSet<String>>>,
    key: String,
}

impl Drop for InFlightSlot {
    fn drop(&mut self) {
        if let Ok(mut set) = self.in_flight.write() {
            set.remove(&self.key);
        }
    }
}

impl InFlightGuard {
    pub fn new() -> Self {
        Self {
            in_flight: Arc::new(RwLock::new(std::collections::HashSet::new())),
        }
    }

    /// Attempts to acquire an in-flight slot for `key`.
    /// Returns `Some(InFlightSlot)` if no request is currently in flight for `key`,
    /// or `None` if an operation for `key` is already in flight.
    pub fn acquire(&self, key: &str) -> Option<InFlightSlot> {
        let mut set = self
            .in_flight
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if set.insert(key.to_string()) {
            Some(InFlightSlot {
                in_flight: self.in_flight.clone(),
                key: key.to_string(),
            })
        } else {
            None
        }
    }

    /// Checks whether an operation is currently in flight for `key`.
    pub fn is_in_flight(&self, key: &str) -> bool {
        let set = self
            .in_flight
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        set.contains(key)
    }
}

// ---------------------------------------------------------------------------
// Unit & Integration Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
