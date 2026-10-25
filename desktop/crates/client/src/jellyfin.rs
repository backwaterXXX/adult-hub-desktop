use crate::models::{
    AuthResult, PlaybackProgressInfo, PlaybackResolution, PlaybackStartInfo, PlaybackStopInfo,
    QuickConnectInit, QuickConnectStatus, ServerInfo, TvItem, TvItemsPage, TvLibrary, TvPersonRole,
};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum JellyfinError {
    #[error("unauthorized (HTTP {0})")]
    Unauthorized(u16),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("api error: {0}")]
    Api(String),
}

impl JellyfinError {
    pub fn is_unauthorized(&self) -> bool {
        match self {
            JellyfinError::Unauthorized(_) => true,
            JellyfinError::Http(e) => e
                .status()
                .is_some_and(|s| s.as_u16() == 401 || s.as_u16() == 403),
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, JellyfinError>;

#[derive(Clone)]
pub struct JellyfinClient {
    client: Client,
    client_name: String,
    device_name: String,
    device_id: String,
    version: String,
}

#[derive(Debug, Clone, Default)]
pub struct GetItemsQuery<'a> {
    pub parent_id: Option<&'a str>,
    pub start_index: usize,
    pub limit: usize,
    pub sort_by: &'a str,
    pub sort_descending: bool,
    pub search_term: Option<&'a str>,
    pub name_starts_with: Option<&'a str>,
    pub person: Option<&'a str>,
    pub person_ids: Option<&'a str>,
    pub studio: Option<&'a str>,
    pub genre: Option<&'a str>,
    pub is_favorite: Option<bool>,
    pub is_hd: Option<bool>,
    pub is_unplayed: bool,
    pub include_item_types: Option<&'a str>,
    pub person_types: Option<&'a str>,
}

impl JellyfinClient {
    pub fn new(device_id: impl Into<String>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();

        Self {
            client,
            client_name: crate::CLIENT_NAME.into(),
            device_name: "Desktop".into(),
            device_id: device_id.into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    pub fn clean_url(base_url: &str) -> String {
        match crate::policy::ServiceUrlPolicy::inspect(base_url, false) {
            crate::policy::ServiceUrlValidation::Valid(u) => u.normalized,
            _ => base_url.trim().trim_end_matches('/').to_string(),
        }
    }

    fn auth_header(&self, token: Option<&str>) -> String {
        let base = format!(
            "MediaBrowser Client=\"{}\", Device=\"{}\", DeviceId=\"{}\", Version=\"{}\"",
            self.client_name, self.device_name, self.device_id, self.version
        );
        match token {
            Some(t) if !t.is_empty() => format!("{base}, Token=\"{t}\""),
            _ => base,
        }
    }

    fn headers(&self, token: Option<&str>) -> HeaderMap {
        let mut map = HeaderMap::new();
        if let Ok(val) = HeaderValue::from_str(&self.auth_header(token)) {
            map.insert("X-Emby-Authorization", val.clone());
            map.insert(reqwest::header::AUTHORIZATION, val);
        }
        if let Some(t) = token {
            if let Ok(val) = HeaderValue::from_str(t) {
                map.insert("X-Emby-Token", val);
            }
        }
        map
    }

    fn check_res(res: reqwest::blocking::Response) -> Result<reqwest::blocking::Response> {
        let code = res.status().as_u16();
        if code == 401 || code == 403 {
            return Err(JellyfinError::Unauthorized(code));
        }
        res.error_for_status().map_err(JellyfinError::from)
    }

    pub fn get_public_system_info(&self, base_url: &str) -> Result<ServerInfo> {
        let base = Self::clean_url(base_url);
        let url = format!("{base}/System/Info/Public");
        let res: PublicSystemInfoDto = self
            .client
            .get(&url)
            .headers(self.headers(None))
            .send()?
            .error_for_status()?
            .json()?;

        Ok(ServerInfo {
            id: res.id.unwrap_or_default(),
            name: res.server_name.unwrap_or_else(|| "Jellyfin".into()),
            version: res.version.unwrap_or_default(),
            url: base,
        })
    }

    pub fn authenticate_by_name(
        &self,
        base_url: &str,
        username: &str,
        password: &str,
    ) -> Result<AuthResult> {
        let base = Self::clean_url(base_url);
        let url = format!("{base}/Users/AuthenticateByName");
        #[derive(Serialize)]
        struct AuthReq<'a> {
            #[serde(rename = "Username")]
            username: &'a str,
            #[serde(rename = "Pw")]
            pw: &'a str,
        }

        let body = AuthReq {
            username,
            pw: password,
        };
        let res: AuthenticateResultDto = self
            .client
            .post(&url)
            .headers(self.headers(None))
            .json(&body)
            .send()?
            .error_for_status()?
            .json()?;

        let token = res
            .access_token
            .ok_or_else(|| JellyfinError::Api("No access token in response".into()))?;
        let user = res
            .user
            .ok_or_else(|| JellyfinError::Api("No user object in response".into()))?;

        Ok(AuthResult {
            access_token: token,
            user_id: user.id,
            username: user.name.unwrap_or_else(|| username.to_string()),
            server_id: res.server_id,
        })
    }

    pub fn quick_connect_initiate(&self, base_url: &str) -> Result<QuickConnectInit> {
        let base = Self::clean_url(base_url);
        let url = format!("{base}/QuickConnect/Initiate");
        #[derive(Deserialize)]
        struct InitDto {
            #[serde(rename = "Code")]
            code: Option<String>,
            #[serde(rename = "Secret")]
            secret: Option<String>,
        }
        let res: InitDto = self
            .client
            .post(&url)
            .headers(self.headers(None))
            .send()?
            .error_for_status()?
            .json()?;

        Ok(QuickConnectInit {
            code: res.code.unwrap_or_default(),
            secret: res.secret.unwrap_or_default(),
        })
    }

    pub fn quick_connect_poll(&self, base_url: &str, secret: &str) -> Result<QuickConnectStatus> {
        let base = Self::clean_url(base_url);
        let url = format!(
            "{base}/QuickConnect/Connect?secret={}",
            crate::urlencoding(secret)
        );
        #[derive(Deserialize)]
        struct ConnectDto {
            #[serde(rename = "Authenticated")]
            authenticated: Option<bool>,
            #[serde(rename = "Secret")]
            secret: Option<String>,
        }
        let res: ConnectDto = self
            .client
            .get(&url)
            .headers(self.headers(None))
            .send()?
            .error_for_status()?
            .json()?;

        Ok(QuickConnectStatus {
            authenticated: res.authenticated.unwrap_or(false),
            secret: res.secret.unwrap_or_else(|| secret.to_string()),
        })
    }

    pub fn quick_connect_authenticate(&self, base_url: &str, secret: &str) -> Result<AuthResult> {
        let base = Self::clean_url(base_url);
        let url = format!("{base}/Users/AuthenticateWithQuickConnect");
        #[derive(Serialize)]
        struct QuickAuthReq<'a> {
            #[serde(rename = "Secret")]
            secret: &'a str,
        }
        let res: AuthenticateResultDto = self
            .client
            .post(&url)
            .headers(self.headers(None))
            .json(&QuickAuthReq { secret })
            .send()?
            .error_for_status()?
            .json()?;

        let token = res
            .access_token
            .ok_or_else(|| JellyfinError::Api("No access token in response".into()))?;
        let user = res
            .user
            .ok_or_else(|| JellyfinError::Api("No user object in response".into()))?;

        Ok(AuthResult {
            access_token: token,
            user_id: user.id,
            username: user.name.unwrap_or_default(),
            server_id: res.server_id,
        })
    }

    pub fn get_user_views(
        &self,
        base_url: &str,
        user_id: &str,
        token: &str,
    ) -> Result<Vec<TvLibrary>> {
        let base = Self::clean_url(base_url);
        let url = format!("{base}/Users/{}/Views", crate::urlencoding(user_id));
        let res: ItemsResponseDto = Self::check_res(
            self.client
                .get(&url)
                .headers(self.headers(Some(token)))
                .send()?,
        )?
        .json()?;

        let libraries = res
            .items
            .into_iter()
            .map(|dto| TvLibrary {
                id: dto.id,
                name: dto.name.unwrap_or_else(|| "Library".into()),
                collection_type: dto.collection_type,
            })
            .collect();
        Ok(libraries)
    }

    pub fn get_resume_items(
        &self,
        base_url: &str,
        user_id: &str,
        token: &str,
        limit: usize,
    ) -> Result<Vec<TvItem>> {
        Ok(self
            .get_resume_items_page(base_url, user_id, token, 0, limit)?
            .items)
    }

    pub fn get_resume_items_page(
        &self,
        base_url: &str,
        user_id: &str,
        token: &str,
        start_index: usize,
        limit: usize,
    ) -> Result<TvItemsPage> {
        let base = Self::clean_url(base_url);
        let url = format!(
            "{base}/UserItems/Resume?userId={}&startIndex={start_index}&limit={limit}&enableTotalRecordCount=true&enableImages=true&enableImageTypes=Primary,Thumb,Backdrop&imageTypeLimit=1&fields=ProviderIds,Overview,Genres,Studios,People,MediaSources,ChildCount,Path,IsPlaceHolder",
            crate::urlencoding(user_id)
        );
        let res: ItemsResponseDto = Self::check_res(
            self.client
                .get(&url)
                .headers(self.headers(Some(token)))
                .send()?,
        )?
        .json()?;

        Ok(TvItemsPage {
            total_record_count: res.total_record_count.unwrap_or(res.items.len()),
            items: res
                .items
                .into_iter()
                .map(map_base_item_to_tv_item)
                .collect(),
            start_index,
        })
    }

    pub fn get_latest_items(
        &self,
        base_url: &str,
        user_id: &str,
        token: &str,
        parent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TvItem>> {
        let base = Self::clean_url(base_url);
        let mut url = format!(
            "{base}/Users/{}/Items/Latest?Limit={limit}&EnableImages=true&EnableImageTypes=Primary,Thumb,Backdrop&ImageTypeLimit=1&Fields=ProviderIds,Overview,Genres,Studios,People,MediaSources,ChildCount,Path,IsPlaceHolder",
            crate::urlencoding(user_id)
        );
        if let Some(pid) = parent_id {
            url.push_str(&format!("&ParentId={}", crate::urlencoding(pid)));
        }
        let res: Vec<BaseItemDto> = Self::check_res(
            self.client
                .get(&url)
                .headers(self.headers(Some(token)))
                .send()?,
        )?
        .json()?;

        Ok(res.into_iter().map(map_base_item_to_tv_item).collect())
    }

    pub fn get_items(
        &self,
        base_url: &str,
        user_id: &str,
        token: &str,
        query: &GetItemsQuery<'_>,
    ) -> Result<TvItemsPage> {
        let base = Self::clean_url(base_url);
        let include_item_types = query.include_item_types.unwrap_or("Movie,Video");
        let mut url = format!(
            "{base}/Users/{}/Items?StartIndex={}&Limit={}&Recursive=true&IncludeItemTypes={}&EnableImages=true&EnableImageTypes=Primary,Thumb,Backdrop&ImageTypeLimit=1&Fields=ProviderIds,Overview,Genres,Studios,People,MediaSources,ChildCount,Path,IsPlaceHolder,CommunityRating",
            crate::urlencoding(user_id),
            query.start_index,
            query.limit,
            crate::urlencoding(include_item_types)
        );
        if let Some(pid) = query.parent_id {
            url.push_str(&format!("&ParentId={}", crate::urlencoding(pid)));
        }
        if let Some(person_types) = query.person_types {
            url.push_str(&format!(
                "&PersonTypes={}",
                crate::urlencoding(person_types)
            ));
        }
        if !query.sort_by.is_empty() {
            url.push_str(&format!("&SortBy={}", crate::urlencoding(query.sort_by)));
            let order = if query.sort_descending {
                "Descending"
            } else {
                "Ascending"
            };
            url.push_str(&format!("&SortOrder={order}"));
        }

        if let Some(term) = query.search_term {
            if !term.trim().is_empty() {
                url.push_str(&format!("&SearchTerm={}", crate::urlencoding(term.trim())));
            }
        }
        if let Some(prefix) = query.name_starts_with {
            if !prefix.trim().is_empty() {
                url.push_str(&format!(
                    "&NameStartsWith={}",
                    crate::urlencoding(prefix.trim())
                ));
            }
        }
        if let Some(person) = query.person {
            if !person.trim().is_empty() {
                url.push_str(&format!("&Person={}", crate::urlencoding(person.trim())));
            }
        }
        if let Some(person_ids) = query.person_ids {
            if !person_ids.trim().is_empty() {
                url.push_str(&format!(
                    "&PersonIds={}",
                    crate::urlencoding(person_ids.trim())
                ));
            }
        }
        if let Some(studio) = query.studio {
            if !studio.trim().is_empty() {
                url.push_str(&format!("&Studios={}", crate::urlencoding(studio.trim())));
            }
        }
        if let Some(genre) = query.genre {
            if !genre.trim().is_empty() {
                url.push_str(&format!("&Genres={}", crate::urlencoding(genre.trim())));
            }
        }
        if let Some(fav) = query.is_favorite {
            url.push_str(&format!("&IsFavorite={fav}"));
        }
        if let Some(is_hd) = query.is_hd {
            url.push_str(&format!("&IsHd={is_hd}"));
        }
        if query.is_unplayed {
            url.push_str("&Filters=IsUnplayed");
        }

        let res: ItemsResponseDto = Self::check_res(
            self.client
                .get(&url)
                .headers(self.headers(Some(token)))
                .send()?,
        )?
        .json()?;

        let total = res.total_record_count.unwrap_or(res.items.len());
        let items = res
            .items
            .into_iter()
            .map(map_base_item_to_tv_item)
            .collect();

        Ok(TvItemsPage {
            items,
            total_record_count: total,
            start_index: query.start_index,
        })
    }

    pub fn get_person(
        &self,
        base_url: &str,
        user_id: &str,
        token: &str,
        name: &str,
    ) -> Result<crate::models::TvPersonDetails> {
        let base = Self::clean_url(base_url);
        let url = format!(
            "{base}/Persons/{}?userId={}",
            crate::urlencoding(name.trim()),
            crate::urlencoding(user_id.trim())
        );
        let res = Self::check_res(
            self.client
                .get(&url)
                .headers(self.headers(Some(token)))
                .send()?,
        )?;
        let dto: BaseItemDto = res.json()?;
        Ok(crate::models::TvPersonDetails {
            id: dto.id,
            name: dto.name.unwrap_or_else(|| name.to_string()),
            overview: dto.overview,
            primary_image_tag: dto
                .image_tags
                .as_ref()
                .and_then(|m| m.get("Primary").cloned()),
            child_count: dto.child_count,
        })
    }

    pub fn get_similar_items(
        &self,
        base_url: &str,
        token: &str,
        item_id: &str,
        limit: usize,
    ) -> Result<Vec<TvItem>> {
        let base = Self::clean_url(base_url);
        let url = format!(
            "{base}/Items/{}/Similar?Limit={limit}&Fields=ProviderIds,Overview,Genres,Studios,People,MediaSources,ChildCount,Path,IsPlaceHolder",
            crate::urlencoding(item_id)
        );
        let res: ItemsResponseDto = Self::check_res(
            self.client
                .get(&url)
                .headers(self.headers(Some(token)))
                .send()?,
        )?
        .json()?;

        Ok(res
            .items
            .into_iter()
            .map(map_base_item_to_tv_item)
            .collect())
    }

    pub fn image_url(
        base_url: &str,
        item_id: &str,
        image_type: &str,
        max_width: u32,
        tag: Option<&str>,
    ) -> String {
        let base = Self::clean_url(base_url);
        let tag_param = match tag {
            Some(t) if !t.trim().is_empty() => format!("&tag={}", crate::urlencoding(t)),
            _ => String::new(),
        };
        // Keep the URL shape identical to the Android client. Jellyfin accepts
        // Backdrop without an explicit index, including when served through a
        // reverse proxy that does not route `/Backdrop/0` correctly.
        // Keep this URL compatible with the Android client and Jellyfin
        // proxies. Some servers reject or fail to transcode `format=jpg` for
        // individual library items, while the default image representation is
        // already supported by our decoder (including WebP).
        format!(
            "{base}/Items/{}/Images/{image_type}?maxWidth={max_width}{tag_param}",
            crate::urlencoding(item_id)
        )
    }

    pub fn stream_url(base_url: &str, item_id: &str) -> String {
        let base = Self::clean_url(base_url);
        format!("{base}/Videos/{item_id}/stream?static=true")
    }

    /// Ask Jellyfin to select a playable source before starting a library item.
    /// This keeps the stream URL, headers, and session identifiers aligned with
    /// the server's direct-play / direct-stream / transcode decision.
    pub fn resolve_playback(
        &self,
        base_url: &str,
        user_id: &str,
        token: &str,
        item_id: &str,
    ) -> Result<PlaybackResolution> {
        let base = Self::clean_url(base_url);
        let url = format!("{base}/Items/{item_id}/PlaybackInfo");
        let response: PlaybackInfoResponseDto = Self::check_res(
            self.client
                .post(&url)
                .query(&[("userId", user_id)])
                .headers(self.headers(Some(token)))
                .json(&PlaybackInfoRequestDto {
                    user_id: user_id.to_owned(),
                    enable_direct_play: true,
                    enable_direct_stream: true,
                    enable_transcoding: true,
                })
                .send()?,
        )?
        .json()?;

        let source = response
            .media_sources
            .unwrap_or_default()
            .into_iter()
            .filter(|source| {
                source.supports_direct_play.unwrap_or(false)
                    || source.supports_direct_stream.unwrap_or(false)
                    || source.supports_transcoding.unwrap_or(false)
            })
            .max_by_key(|source| {
                (
                    source.supports_direct_play.unwrap_or(false),
                    source.supports_direct_stream.unwrap_or(false),
                    source.bitrate.unwrap_or(0),
                )
            })
            .ok_or_else(|| {
                JellyfinError::Api("Jellyfin returned no playable media source".into())
            })?;

        let play_method = if source.supports_direct_play.unwrap_or(false) {
            crate::models::PlayMethod::DirectPlay
        } else if source.supports_direct_stream.unwrap_or(false) {
            crate::models::PlayMethod::DirectStream
        } else {
            crate::models::PlayMethod::Transcode
        };
        let stream_url = source
            .transcoding_url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .map(|path| {
                if path.starts_with("http://") || path.starts_with("https://") {
                    path.to_owned()
                } else {
                    format!("{base}/{}", path.trim_start_matches('/'))
                }
            })
            .unwrap_or_else(|| {
                let mut url = format!("{base}/Videos/{item_id}/stream?static=true");
                if let Some(source_id) = source.id.as_deref() {
                    url.push_str("&mediaSourceId=");
                    url.push_str(&crate::urlencoding(source_id));
                }
                if let Some(play_session_id) = response.play_session_id.as_deref() {
                    url.push_str("&PlaySessionId=");
                    url.push_str(&crate::urlencoding(play_session_id));
                }
                url
            });

        Ok(PlaybackResolution {
            stream_url,
            media_source_id: source.id,
            play_session_id: response.play_session_id,
            required_headers: source
                .required_http_headers
                .into_iter()
                .map(|(name, value)| format!("{name}: {value}"))
                .collect(),
            play_method,
        })
    }

    pub fn stream_headers(&self, token: Option<&str>) -> Vec<String> {
        let mut headers = Vec::new();
        let auth = self.auth_header(token);
        headers.push(format!("Authorization: {auth}"));
        headers.push(format!("X-Emby-Authorization: {auth}"));
        if let Some(t) = token {
            if !t.is_empty() {
                headers.push(format!("X-Emby-Token: {t}"));
            }
        }
        headers
    }

    pub fn report_playback_start(
        &self,
        base_url: &str,
        token: &str,
        info: &PlaybackStartInfo,
    ) -> Result<()> {
        if uuid::Uuid::parse_str(&info.item_id).is_err() {
            return Ok(());
        }
        let base = Self::clean_url(base_url);
        let url = format!("{base}/Sessions/Playing");
        Self::check_res(
            self.client
                .post(&url)
                .headers(self.headers(Some(token)))
                .json(info)
                .send()?,
        )?;
        Ok(())
    }

    pub fn report_playback_progress(
        &self,
        base_url: &str,
        token: &str,
        info: &PlaybackProgressInfo,
    ) -> Result<()> {
        if uuid::Uuid::parse_str(&info.item_id).is_err() {
            return Ok(());
        }
        let base = Self::clean_url(base_url);
        let url = format!("{base}/Sessions/Playing/Progress");
        Self::check_res(
            self.client
                .post(&url)
                .headers(self.headers(Some(token)))
                .json(info)
                .send()?,
        )?;
        Ok(())
    }

    pub fn report_playback_stopped(
        &self,
        base_url: &str,
        token: &str,
        info: &PlaybackStopInfo,
    ) -> Result<()> {
        if uuid::Uuid::parse_str(&info.item_id).is_err() {
            return Ok(());
        }
        let base = Self::clean_url(base_url);
        let url = format!("{base}/Sessions/Playing/Stopped");
        Self::check_res(
            self.client
                .post(&url)
                .headers(self.headers(Some(token)))
                .json(info)
                .send()?,
        )?;
        Ok(())
    }

    pub fn get_item_details(
        &self,
        base_url: &str,
        user_id: &str,
        token: &str,
        item_id: &str,
    ) -> Result<TvItem> {
        let base = Self::clean_url(base_url);
        let url = format!(
            "{base}/Users/{}/Items/{}?EnableImages=true&EnableImageTypes=Primary,Thumb,Backdrop&ImageTypeLimit=1&Fields=ProviderIds,Overview,Genres,Studios,People,MediaSources,ChildCount,Path,IsPlaceHolder",
            crate::urlencoding(user_id),
            crate::urlencoding(item_id)
        );
        let res: BaseItemDto = Self::check_res(
            self.client
                .get(&url)
                .headers(self.headers(Some(token)))
                .send()?,
        )?
        .json()?;
        Ok(map_base_item_to_tv_item(res))
    }

    pub fn toggle_favorite(
        &self,
        base_url: &str,
        user_id: &str,
        token: &str,
        item_id: &str,
        is_favorite: bool,
    ) -> Result<()> {
        if uuid::Uuid::parse_str(item_id).is_err() {
            return Ok(());
        }
        let base = Self::clean_url(base_url);
        let url = format!(
            "{base}/Users/{}/FavoriteItems/{}",
            crate::urlencoding(user_id),
            crate::urlencoding(item_id)
        );
        let req = if is_favorite {
            self.client.post(&url)
        } else {
            self.client.delete(&url)
        };
        Self::check_res(req.headers(self.headers(Some(token))).send()?)?;
        Ok(())
    }

    pub fn toggle_played(
        &self,
        base_url: &str,
        user_id: &str,
        token: &str,
        item_id: &str,
        is_played: bool,
    ) -> Result<()> {
        if uuid::Uuid::parse_str(item_id).is_err() {
            return Ok(());
        }
        let base = Self::clean_url(base_url);
        let url = format!(
            "{base}/Users/{}/PlayedItems/{}",
            crate::urlencoding(user_id),
            crate::urlencoding(item_id)
        );
        let req = if is_played {
            self.client.post(&url)
        } else {
            self.client.delete(&url)
        };
        Self::check_res(req.headers(self.headers(Some(token))).send()?)?;
        Ok(())
    }
}

// Internal Jellyfin DTOs
#[derive(Deserialize)]
struct PublicSystemInfoDto {
    #[serde(rename = "Id")]
    id: Option<String>,
    #[serde(rename = "ServerName")]
    server_name: Option<String>,
    #[serde(rename = "Version")]
    version: Option<String>,
}

#[derive(Deserialize)]
struct AuthenticateResultDto {
    #[serde(rename = "AccessToken")]
    access_token: Option<String>,
    #[serde(rename = "ServerId")]
    server_id: Option<String>,
    #[serde(rename = "User")]
    user: Option<UserDto>,
}

#[derive(Deserialize)]
struct UserDto {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name")]
    name: Option<String>,
}

#[derive(Deserialize)]
struct ItemsResponseDto {
    #[serde(rename = "Items", default)]
    items: Vec<BaseItemDto>,
    #[serde(rename = "TotalRecordCount")]
    total_record_count: Option<usize>,
}

#[derive(Deserialize)]
struct BaseItemDto {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Overview")]
    overview: Option<String>,
    #[serde(rename = "ProductionYear")]
    production_year: Option<i32>,
    #[serde(rename = "RunTimeTicks")]
    run_time_ticks: Option<i64>,
    #[serde(rename = "Genres")]
    genres: Option<Vec<String>>,
    #[serde(rename = "Tags")]
    tags: Option<Vec<String>>,
    #[serde(rename = "Studios")]
    studios: Option<Vec<StudioDto>>,
    #[serde(rename = "People")]
    people: Option<Vec<PersonDto>>,
    #[serde(rename = "ImageTags")]
    image_tags: Option<std::collections::HashMap<String, String>>,
    #[serde(rename = "BackdropImageTags")]
    backdrop_image_tags: Option<Vec<String>>,
    #[serde(rename = "UserData")]
    user_data: Option<UserDataDto>,
    #[serde(rename = "ProviderIds")]
    provider_ids: Option<std::collections::HashMap<String, String>>,
    #[serde(rename = "MediaSources")]
    media_sources: Option<Vec<MediaSourceDto>>,
    #[serde(rename = "Container")]
    container: Option<String>,
    #[serde(rename = "Width")]
    width: Option<u32>,
    #[serde(rename = "Height")]
    height: Option<u32>,
    #[serde(rename = "ChildCount")]
    child_count: Option<u32>,
    #[serde(rename = "CollectionType")]
    collection_type: Option<String>,
    #[serde(rename = "CommunityRating")]
    community_rating: Option<f32>,
}

#[derive(Deserialize)]
struct StudioDto {
    #[serde(rename = "Name")]
    name: Option<String>,
}

#[derive(Deserialize)]
struct PersonDto {
    #[serde(rename = "Id")]
    id: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Role")]
    role: Option<String>,
    #[serde(rename = "Type")]
    person_type: Option<String>,
    #[serde(rename = "PrimaryImageTag")]
    primary_image_tag: Option<String>,
}

#[derive(Deserialize)]
struct UserDataDto {
    #[serde(rename = "IsFavorite")]
    is_favorite: Option<bool>,
    #[serde(rename = "PlaybackPositionTicks")]
    playback_position_ticks: Option<i64>,
    #[serde(rename = "PlayedPercentage")]
    played_percentage: Option<f64>,
    #[serde(rename = "Played")]
    played: Option<bool>,
}

#[derive(Deserialize)]
struct MediaSourceDto {
    #[serde(rename = "Id")]
    id: Option<String>,
    #[serde(rename = "Size")]
    size: Option<i64>,
    #[serde(rename = "Path")]
    path: Option<String>,
    #[serde(rename = "Container")]
    container: Option<String>,
    #[serde(rename = "IsPlaceHolder")]
    is_place_holder: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct PlaybackInfoRequestDto {
    user_id: String,
    enable_direct_play: bool,
    enable_direct_stream: bool,
    enable_transcoding: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PlaybackInfoResponseDto {
    play_session_id: Option<String>,
    media_sources: Option<Vec<PlaybackMediaSourceDto>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PlaybackMediaSourceDto {
    id: Option<String>,
    bitrate: Option<i64>,
    supports_direct_play: Option<bool>,
    supports_direct_stream: Option<bool>,
    supports_transcoding: Option<bool>,
    transcoding_url: Option<String>,
    #[serde(default)]
    required_http_headers: std::collections::HashMap<String, String>,
}

fn map_base_item_to_tv_item(dto: BaseItemDto) -> TvItem {
    let primary_image_tag = dto
        .image_tags
        .as_ref()
        .and_then(|map| map.get("Primary").cloned());
    let thumb_image_tag = dto
        .image_tags
        .as_ref()
        .and_then(|map| map.get("Thumb").cloned());
    let backdrop_image_tag = dto
        .backdrop_image_tags
        .and_then(|vec| vec.into_iter().next());

    let people = dto
        .people
        .unwrap_or_default()
        .into_iter()
        .map(|p| TvPersonRole {
            id: p.id.unwrap_or_default(),
            name: p.name.unwrap_or_default(),
            role: p.role,
            person_type: p.person_type,
            primary_image_tag: p.primary_image_tag,
        })
        .collect();

    let studios = dto
        .studios
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| s.name)
        .collect();

    let provider_video_id = dto
        .provider_ids
        .and_then(|mut map| map.remove("telegram").or_else(|| map.remove("Telegram")));

    let (media_source_id, file_size_bytes, is_placeholder_source) =
        if let Some(sources) = dto.media_sources {
            match sources.into_iter().next() {
                Some(first) => {
                    // A `.strm` referral is a placeholder: Jellyfin reports the
                    // size of the tiny text file, not the media it points at.
                    let placeholder = first.is_place_holder.unwrap_or(false)
                        || first
                            .path
                            .as_deref()
                            .is_some_and(|path| path.to_ascii_lowercase().ends_with(".strm"))
                        || first
                            .container
                            .as_deref()
                            .is_some_and(|container| container.eq_ignore_ascii_case("strm"));
                    let size = if placeholder {
                        None
                    } else {
                        first.size.filter(|size| *size > 0)
                    };
                    (first.id, size, placeholder)
                }
                None => (None, None, false),
            }
        } else {
            (None, None, false)
        };

    let user_data = dto.user_data;
    let is_favorite = user_data
        .as_ref()
        .and_then(|u| u.is_favorite)
        .unwrap_or(false);
    let playback_position_ticks = user_data
        .as_ref()
        .and_then(|u| u.playback_position_ticks)
        .unwrap_or(0);
    let played_percentage = user_data.as_ref().and_then(|u| u.played_percentage);
    let is_played = user_data.as_ref().and_then(|u| u.played).unwrap_or(false);

    TvItem {
        id: dto.id,
        title: dto.name.unwrap_or_else(|| "Untitled".into()),
        source_provider: None,
        overview: dto.overview,
        year: dto.production_year,
        runtime_ticks: dto.run_time_ticks,
        genres: dto.genres.unwrap_or_default(),
        tags: dto.tags.unwrap_or_default(),
        studios,
        people,
        primary_image_tag,
        backdrop_image_tag,
        thumb_image_tag,
        child_count: dto.child_count,
        is_favorite,
        playback_position_ticks,
        played_percentage,
        is_played,
        provider_video_id,
        media_source_id,
        container: dto.container,
        file_size_bytes,
        is_placeholder_source,
        width: dto.width,
        height: dto.height,
        community_rating: dto.community_rating,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_jellyfin_base_item_to_tv_item() {
        let json = r#"{
            "Id": "item-123",
            "Name": "Sample Video",
            "Overview": "A test overview",
            "ProductionYear": 2024,
            "RunTimeTicks": 72000000000,
            "Genres": ["Action", "Feature"],
            "Tags": ["HD", "4K"],
            "Studios": [{"Name": "Studio A"}],
            "People": [{"Id": "p1", "Name": "Jane Doe", "Role": "Lead", "Type": "Actor", "PrimaryImageTag": "tag1"}],
            "ImageTags": {"Primary": "prim1", "Thumb": "th1"},
            "BackdropImageTags": ["bd1"],
            "UserData": {
                "IsFavorite": true,
                "PlaybackPositionTicks": 36000000000,
                "PlayedPercentage": 50.0,
                "Played": false
            },
            "ProviderIds": {"telegram": "tg-999"},
            "MediaSources": [{"Id": "ms1", "Size": 104857600}],
            "Container": "mp4",
            "Width": 1920,
            "Height": 1080
        }"#;

        let dto: BaseItemDto = serde_json::from_str(json).unwrap();
        let item = map_base_item_to_tv_item(dto);

        assert_eq!(item.id, "item-123");
        assert_eq!(item.title, "Sample Video");
        assert_eq!(item.year, Some(2024));
        assert_eq!(item.runtime_minutes(), Some(120));
        assert_eq!(item.formatted_duration(), "2h 00m");
        assert_eq!(item.progress_fraction(), 0.5);
        assert!(item.is_favorite);
        assert!(!item.is_played);
        // A `telegram` ProviderId makes the item stream through the resolver,
        // but Home/details resume state additionally comes from the server, so
        // progress must be read from both places.
        assert!(item.is_telegram());
        assert_eq!(item.provider_video_id.as_deref(), Some("tg-999"));
        assert_eq!(item.media_source_id.as_deref(), Some("ms1"));
        assert_eq!(item.file_size_bytes, Some(104857600));
        assert!(!item.is_placeholder_source);
        assert_eq!(item.format_media_size(), "100.0 MB");
        assert_eq!(item.studios, vec!["Studio A"]);
        assert_eq!(item.people.len(), 1);
        assert_eq!(item.people[0].name, "Jane Doe");
    }

    #[test]
    fn referral_size_is_rejected_even_without_the_placeholder_flag() {
        // Regression for the "Doctors…" Details screen: the server reported the
        // 102-byte pointer body without IsPlaceHolder, so the placeholder guard
        // alone let "102 B" through.
        let json = r#"{
            "Id": "strm-item-3",
            "Name": "Doctors",
            "MediaSources": [{"Id": "ms4", "Size": 102, "Path": "/mnt/library/Doctors.mkv.strm?v=1"}]
        }"#;

        let dto: BaseItemDto = serde_json::from_str(json).unwrap();
        let item = map_base_item_to_tv_item(dto);
        assert_eq!(item.media_size_bytes(), None);
        assert_eq!(item.format_media_size(), "Unavailable");

        // A server that reports neither a path nor the flag still cannot leak
        // the pointer size through the display guard.
        let json = r#"{
            "Id": "strm-item-4",
            "Name": "Doctors",
            "MediaSources": [{"Id": "ms5", "Size": 102}]
        }"#;
        let dto: BaseItemDto = serde_json::from_str(json).unwrap();
        let item = map_base_item_to_tv_item(dto);
        assert_eq!(item.media_size_bytes(), None);
    }

    #[test]
    fn strm_referral_size_is_never_reported_as_media_size() {
        let json = r#"{
            "Id": "strm-item",
            "Name": "Masonic Boys Scene",
            "RunTimeTicks": 21000000000,
            "MediaSources": [{
                "Id": "ms2",
                "Size": 103,
                "Path": "/mnt/library/Masonic Boys - The Calling.strm",
                "Container": "strm",
                "IsPlaceHolder": true
            }]
        }"#;

        let dto: BaseItemDto = serde_json::from_str(json).unwrap();
        let item = map_base_item_to_tv_item(dto);

        assert_eq!(item.media_source_id.as_deref(), Some("ms2"));
        assert_eq!(item.file_size_bytes, None);
        assert!(item.is_placeholder_source);
        assert_eq!(item.media_size_bytes(), None);
        assert_eq!(item.format_media_size(), "Unavailable");
    }

    #[test]
    fn strm_source_detected_from_extension_without_placeholder_flag() {
        let json = r#"{
            "Id": "strm-item-2",
            "Name": "Referral",
            "MediaSources": [{"Id": "ms3", "Size": 103, "Path": "/mnt/library/Scene.STRM"}]
        }"#;

        let dto: BaseItemDto = serde_json::from_str(json).unwrap();
        let item = map_base_item_to_tv_item(dto);

        assert!(item.is_placeholder_source);
        assert_eq!(item.media_size_bytes(), None);
    }

    #[test]
    fn formats_image_url() {
        let url = JellyfinClient::image_url(
            "http://192.168.1.50:8096/",
            "item-456",
            "Primary",
            480,
            Some("tag123"),
        );
        assert_eq!(
            url,
            "http://192.168.1.50:8096/Items/item-456/Images/Primary?maxWidth=480&tag=tag123"
        );

        let backdrop = JellyfinClient::image_url(
            "http://192.168.1.50:8096",
            "item-456",
            "Backdrop",
            480,
            Some("tag+with&symbols"),
        );
        assert_eq!(
            backdrop,
            "http://192.168.1.50:8096/Items/item-456/Images/Backdrop?maxWidth=480&tag=tag%2Bwith%26symbols"
        );
    }

    #[test]
    fn playback_reporting_skips_non_uuid() {
        let client = JellyfinClient::new("test-device");
        let start_info = PlaybackStartInfo {
            item_id: "telegram:123456".into(),
            media_source_id: None,
            position_ticks: 10_000_000,
            play_session_id: None,
            can_seek: true,
            is_muted: false,
            is_paused: false,
            repeat_mode: "RepeatNone".into(),
            playback_order: "Default".into(),
            play_method: crate::models::PlayMethod::DirectPlay,
        };
        // Non-UUID ID must return Ok(()) without attempting network connection
        assert!(
            client
                .report_playback_start("http://invalid.local", "token", &start_info)
                .is_ok()
        );

        let progress_info = PlaybackProgressInfo {
            item_id: "telegram:123456".into(),
            media_source_id: None,
            position_ticks: 50_000_000,
            play_session_id: None,
            can_seek: true,
            is_muted: false,
            is_paused: false,
            repeat_mode: "RepeatNone".into(),
            playback_order: "Default".into(),
            play_method: crate::models::PlayMethod::DirectPlay,
        };
        assert!(
            client
                .report_playback_progress("http://invalid.local", "token", &progress_info)
                .is_ok()
        );

        let stop_info = PlaybackStopInfo {
            item_id: "telegram:123456".into(),
            media_source_id: None,
            position_ticks: 60_000_000,
            play_session_id: None,
            failed: false,
        };
        assert!(
            client
                .report_playback_stopped("http://invalid.local", "token", &stop_info)
                .is_ok()
        );
    }

    #[test]
    fn serializes_playback_start_info() {
        let start_info = PlaybackStartInfo {
            item_id: "a1b2c3d4-e5f6-7890-abcd-ef1234567890".into(),
            media_source_id: Some("src-1".into()),
            position_ticks: 150_000_000,
            play_session_id: Some("session-abc".into()),
            can_seek: true,
            is_muted: false,
            is_paused: false,
            repeat_mode: "RepeatNone".into(),
            playback_order: "Default".into(),
            play_method: crate::models::PlayMethod::DirectPlay,
        };
        let json = serde_json::to_string(&start_info).unwrap();
        assert!(json.contains("\"ItemId\":\"a1b2c3d4-e5f6-7890-abcd-ef1234567890\""));
        assert!(json.contains("\"PositionTicks\":150000000"));
        assert!(json.contains("\"PlayMethod\":\"DirectPlay\""));
    }

    #[test]
    #[ignore = "requires ADULT_HUB_TEST_JELLYFIN_HOST and network access"]
    fn connects_to_configured_jellyfin_host() {
        let host = std::env::var("ADULT_HUB_TEST_JELLYFIN_HOST")
            .expect("set ADULT_HUB_TEST_JELLYFIN_HOST to a reachable Jellyfin host");
        let client = JellyfinClient::new("test-device");
        let info = client.get_public_system_info(&host).unwrap();
        assert!(!info.name.is_empty());
        assert!(info.url.starts_with("https://"));

        let qc = client.quick_connect_initiate(&host).unwrap();
        assert!(!qc.code.is_empty());
        assert!(!qc.secret.is_empty());
    }

    #[test]
    fn test_unauthorized_error_detection() {
        let err401 = JellyfinError::Unauthorized(401);
        assert!(err401.is_unauthorized());
        let err403 = JellyfinError::Unauthorized(403);
        assert!(err403.is_unauthorized());
        let err_api = JellyfinError::Api("general error".into());
        assert!(!err_api.is_unauthorized());
    }
}
