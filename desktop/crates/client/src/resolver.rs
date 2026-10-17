use crate::models::{CategoryDto, PerformerBioDto, TelegramHistoryResponseDto, VideoDto};
use reqwest::blocking::Client;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ResolverError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("api error: {0}")]
    Api(String),
}

pub type Result<T> = std::result::Result<T, ResolverError>;

#[derive(Clone)]
pub struct ResolverClient {
    client: Client,
}

impl ResolverClient {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self { client }
    }

    pub fn get_categories(&self, base_url: &str) -> Result<Vec<CategoryDto>> {
        let url = format!("{}/admin/api/categories", base_url.trim_end_matches('/'));
        let res: Vec<CategoryDto> = self.client.get(&url).send()?.error_for_status()?.json()?;
        Ok(res)
    }

    pub fn get_category_videos(
        &self,
        base_url: &str,
        channel_id: &str,
        page: usize,
        sort: &str,
        min_duration: Option<usize>,
    ) -> Result<Vec<VideoDto>> {
        let mut url = format!(
            "{}/admin/api/category/{channel_id}?page={page}&sort={sort}",
            base_url.trim_end_matches('/')
        );
        if let Some(dur) = min_duration {
            url.push_str(&format!("&min_duration={dur}"));
        }
        let res: Vec<VideoDto> = self.client.get(&url).send()?.error_for_status()?.json()?;
        Ok(res)
    }

    pub fn get_channel_history(
        &self,
        base_url: &str,
        channel_id: &str,
        limit: usize,
        offset_id: Option<i64>,
    ) -> Result<TelegramHistoryResponseDto> {
        let mut url = format!(
            "{}/admin/api/telegram/history/{channel_id}?limit={limit}",
            base_url.trim_end_matches('/')
        );
        if let Some(offset) = offset_id {
            url.push_str(&format!("&offset_id={offset}"));
        }
        let res: TelegramHistoryResponseDto =
            self.client.get(&url).send()?.error_for_status()?.json()?;
        Ok(res)
    }

    pub fn search(&self, base_url: &str, query: &str, provider: &str) -> Result<Vec<VideoDto>> {
        let url = format!(
            "{}/admin/api/search?q={}&provider={provider}",
            base_url.trim_end_matches('/'),
            urlencoding(query.trim())
        );
        let res: Vec<VideoDto> = self.client.get(&url).send()?.error_for_status()?.json()?;
        Ok(res)
    }

    pub fn get_performer_bio(&self, base_url: &str, name: &str) -> Result<PerformerBioDto> {
        let url = format!(
            "{}/admin/api/performer-bio?name={}",
            base_url.trim_end_matches('/'),
            urlencoding(name.trim())
        );
        let res: PerformerBioDto = self.client.get(&url).send()?.error_for_status()?.json()?;
        Ok(res)
    }

    pub fn stream_url(base_url: &str, provider_or_id: &str) -> String {
        let ref_id = if provider_or_id.starts_with("telegram:") {
            provider_or_id.to_string()
        } else {
            format!("telegram:{provider_or_id}")
        };
        format!(
            "{}/admin/play/{ref_id}/stream?mode=proxy",
            base_url.trim_end_matches('/')
        )
    }
}

impl Default for ResolverClient {
    fn default() -> Self {
        Self::new()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resolver_category_dto() {
        let json = r#"[
            {"id": "c1", "title": "Category 1", "slug": "cat-1", "url": "http://resolver/c1"},
            {"id": "c2", "title": "Category 2"}
        ]"#;
        let cats: Vec<CategoryDto> = serde_json::from_str(json).unwrap();
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].title, "Category 1");
        assert_eq!(cats[1].slug, None);
    }

    #[test]
    fn parses_resolver_telegram_history() {
        let json = r#"{
            "videos": [
                {
                    "id": "v1",
                    "provider": "telegram",
                    "provider_video_id": "1001",
                    "title": "Video 1",
                    "duration": 450.5,
                    "thumbnail": "http://thumb/1.jpg"
                }
            ],
            "next_offset_id": 1002,
            "message_count": 1,
            "has_more": true
        }"#;
        let hist: TelegramHistoryResponseDto = serde_json::from_str(json).unwrap();
        assert_eq!(hist.videos.len(), 1);
        assert_eq!(hist.videos[0].title, "Video 1");
        assert_eq!(hist.next_offset_id, Some(1002));
        assert!(hist.has_more);
    }
}
