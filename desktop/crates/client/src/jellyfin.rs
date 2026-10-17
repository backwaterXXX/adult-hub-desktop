use crate::models::{
    AuthResult, PlaybackProgressInfo, PlaybackStartInfo, PlaybackStopInfo, QuickConnectInit,
    QuickConnectStatus, ServerInfo, TvItem, TvItemsPage, TvLibrary, TvPersonRole,
};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum JellyfinError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("api error: {0}")]
    Api(String),
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
}

impl JellyfinClient {
    pub fn new(device_id: impl Into<String>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();

        Self {
            client,
            client_name: "Adult Hub Desktop".into(),
            device_name: "Desktop".into(),
            device_id: device_id.into(),
            version: "0.1.0".into(),
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
            map.insert("X-Emby-Authorization", val);
        }
        if let Some(t) = token {
            if let Ok(val) = HeaderValue::from_str(t) {
                map.insert("X-Emby-Token", val);
            }
        }
        map
    }

    pub fn get_public_system_info(&self, base_url: &str) -> Result<ServerInfo> {
        let url = format!("{}/System/Info/Public", base_url.trim_end_matches('/'));
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
            url: base_url.trim_end_matches('/').to_string(),
        })
    }

    pub fn authenticate_by_name(
        &self,
        base_url: &str,
        username: &str,
        password: &str,
    ) -> Result<AuthResult> {
        let url = format!(
            "{}/Users/AuthenticateByName",
            base_url.trim_end_matches('/')
        );
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
        let url = format!("{}/QuickConnect/Initiate", base_url.trim_end_matches('/'));
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
        let url = format!(
            "{}/QuickConnect/Connect?secret={secret}",
            base_url.trim_end_matches('/')
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
        let url = format!(
            "{}/Users/AuthenticateWithQuickConnect",
            base_url.trim_end_matches('/')
        );
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
        let url = format!("{}/Users/{user_id}/Views", base_url.trim_end_matches('/'));
        let res: ItemsResponseDto = self
            .client
            .get(&url)
            .headers(self.headers(Some(token)))
            .send()?
            .error_for_status()?
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
        let url = format!(
            "{}/Users/{user_id}/Items/Resume?Limit={limit}&Fields=ProviderIds,Overview,Genres,Studios,People,MediaSources",
            base_url.trim_end_matches('/')
        );
        let res: ItemsResponseDto = self
            .client
            .get(&url)
            .headers(self.headers(Some(token)))
            .send()?
            .error_for_status()?
            .json()?;

        Ok(res
            .items
            .into_iter()
            .map(map_base_item_to_tv_item)
            .collect())
    }

    pub fn get_latest_items(
        &self,
        base_url: &str,
        user_id: &str,
        token: &str,
        parent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TvItem>> {
        let mut url = format!(
            "{}/Users/{user_id}/Items/Latest?Limit={limit}&Fields=ProviderIds,Overview,Genres,Studios,People,MediaSources",
            base_url.trim_end_matches('/')
        );
        if let Some(pid) = parent_id {
            url.push_str(&format!("&ParentId={pid}"));
        }
        let res: Vec<BaseItemDto> = self
            .client
            .get(&url)
            .headers(self.headers(Some(token)))
            .send()?
            .error_for_status()?
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
        let mut url = format!(
            "{}/Users/{user_id}/Items?StartIndex={}&Limit={}&Recursive=true&IncludeItemTypes=Movie&Fields=ProviderIds,Overview,Genres,Studios,People,MediaSources",
            base_url.trim_end_matches('/'),
            query.start_index,
            query.limit
        );
        if let Some(pid) = query.parent_id {
            url.push_str(&format!("&ParentId={pid}"));
        }
        if !query.sort_by.is_empty() {
            url.push_str(&format!("&SortBy={}", query.sort_by));
        }
        let order = if query.sort_descending {
            "Descending"
        } else {
            "Ascending"
        };
        url.push_str(&format!("&SortOrder={order}"));

        if let Some(term) = query.search_term {
            if !term.trim().is_empty() {
                url.push_str(&format!("&SearchTerm={}", urlencoding(term.trim())));
            }
        }

        let res: ItemsResponseDto = self
            .client
            .get(&url)
            .headers(self.headers(Some(token)))
            .send()?
            .error_for_status()?
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

    pub fn get_similar_items(
        &self,
        base_url: &str,
        token: &str,
        item_id: &str,
        limit: usize,
    ) -> Result<Vec<TvItem>> {
        let url = format!(
            "{}/Items/{item_id}/Similar?Limit={limit}&Fields=ProviderIds,Overview,Genres,Studios,People,MediaSources",
            base_url.trim_end_matches('/')
        );
        let res: ItemsResponseDto = self
            .client
            .get(&url)
            .headers(self.headers(Some(token)))
            .send()?
            .error_for_status()?
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
        let tag_param = match tag {
            Some(t) if !t.trim().is_empty() => format!("&tag={t}"),
            _ => String::new(),
        };
        format!(
            "{}/Items/{item_id}/Images/{image_type}?maxWidth={max_width}{tag_param}",
            base_url.trim_end_matches('/')
        )
    }

    pub fn stream_url(base_url: &str, item_id: &str) -> String {
        format!(
            "{}/Videos/{item_id}/stream?static=true",
            base_url.trim_end_matches('/')
        )
    }

    pub fn stream_headers(&self, token: Option<&str>) -> Vec<String> {
        let mut headers = Vec::new();
        headers.push(format!("X-Emby-Authorization: {}", self.auth_header(token)));
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
        let url = format!("{}/Sessions/Playing", base_url.trim_end_matches('/'));
        self.client
            .post(&url)
            .headers(self.headers(Some(token)))
            .json(info)
            .send()?
            .error_for_status()?;
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
        let url = format!("{}/Sessions/Playing/Progress", base_url.trim_end_matches('/'));
        self.client
            .post(&url)
            .headers(self.headers(Some(token)))
            .json(info)
            .send()?
            .error_for_status()?;
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
        let url = format!("{}/Sessions/Playing/Stopped", base_url.trim_end_matches('/'));
        self.client
            .post(&url)
            .headers(self.headers(Some(token)))
            .json(info)
            .send()?
            .error_for_status()?;
        Ok(())
    }

    pub fn get_item_details(
        &self,
        base_url: &str,
        user_id: &str,
        token: &str,
        item_id: &str,
    ) -> Result<TvItem> {
        let url = format!(
            "{}/Users/{user_id}/Items/{item_id}?Fields=ProviderIds,Overview,Genres,Studios,People,MediaSources",
            base_url.trim_end_matches('/')
        );
        let res: BaseItemDto = self
            .client
            .get(&url)
            .headers(self.headers(Some(token)))
            .send()?
            .error_for_status()?
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
        let url = format!(
            "{}/Users/{user_id}/FavoriteItems/{item_id}",
            base_url.trim_end_matches('/')
        );
        let req = if is_favorite {
            self.client.post(&url)
        } else {
            self.client.delete(&url)
        };
        req.headers(self.headers(Some(token)))
            .send()?
            .error_for_status()?;
        Ok(())
    }
}

fn urlencoding(input: &str) -> String {
    let mut encoded = String::new();
    for b in input.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", b));
            }
        }
    }
    encoded
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
    #[serde(rename = "CollectionType")]
    collection_type: Option<String>,
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

    let (media_source_id, file_size_bytes) = if let Some(sources) = dto.media_sources {
        if let Some(first) = sources.into_iter().next() {
            (first.id, first.size)
        } else {
            (None, None)
        }
    } else {
        (None, None)
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
        is_favorite,
        playback_position_ticks,
        played_percentage,
        is_played,
        provider_video_id,
        media_source_id,
        container: dto.container,
        file_size_bytes,
        width: dto.width,
        height: dto.height,
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
        assert!(item.is_telegram());
        assert_eq!(item.provider_video_id.as_deref(), Some("tg-999"));
        assert_eq!(item.media_source_id.as_deref(), Some("ms1"));
        assert_eq!(item.file_size_bytes, Some(104857600));
        assert_eq!(item.studios, vec!["Studio A"]);
        assert_eq!(item.people.len(), 1);
        assert_eq!(item.people[0].name, "Jane Doe");
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
}
