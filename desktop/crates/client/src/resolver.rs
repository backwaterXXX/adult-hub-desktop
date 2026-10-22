use crate::models::{
    CategoryDto, PerformerBioDto, PhashGenerationRequest, PhashGenerationResponse,
    PhashJobStatusResponse, TelegramHistoryResponseDto, VideoDto,
};
use crate::urlencoding;
use reqwest::blocking::Client;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ResolverError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("authorization rejected (HTTP {0})")]
    Auth(u16),
    #[error("not found (HTTP 404)")]
    NotFound,
    #[error("conflict / terminal job failure (HTTP 409)")]
    Conflict,
    #[error("status error (HTTP {0}): {1}")]
    Status(u16, String),
    #[error("api error: {0}")]
    Api(String),
}

impl ResolverError {
    pub fn is_retryable(&self) -> bool {
        match self {
            ResolverError::Http(e) => {
                if e.is_timeout() || e.is_connect() {
                    return true;
                }
                if let Some(status) = e.status() {
                    status.as_u16() == 429 || status.is_server_error()
                } else {
                    true
                }
            }
            ResolverError::Status(code, _) => *code == 429 || *code >= 500,
            _ => false,
        }
    }
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
            "{}/admin/api/category/{}?page={page}&sort={}",
            base_url.trim_end_matches('/'),
            crate::urlencoding(channel_id),
            crate::urlencoding(sort),
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
            "{}/admin/api/telegram/history/{}?limit={limit}",
            base_url.trim_end_matches('/'),
            crate::urlencoding(channel_id),
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
            "{}/admin/api/search?q={}&provider={}",
            base_url.trim_end_matches('/'),
            urlencoding(query.trim()),
            urlencoding(provider.trim())
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

    pub fn generate_phash(
        &self,
        base_url: &str,
        token: &str,
        video_id: &str,
        duration_us: u64,
    ) -> Result<PhashGenerationResponse> {
        let url = format!(
            "{}/admin/api/v2/video-phash/generate",
            base_url.trim_end_matches('/')
        );
        let req_body = PhashGenerationRequest {
            video_id: video_id.to_string(),
            duration_us,
        };
        let response = self
            .client
            .post(&url)
            .header("X-Emby-Token", token)
            .header("Content-Type", "application/json")
            .json(&req_body)
            .send()?;

        let status = response.status();
        let code = status.as_u16();

        if code == 401 || code == 403 {
            return Err(ResolverError::Auth(code));
        }
        if code == 404 {
            return Err(ResolverError::NotFound);
        }
        if code == 409 {
            return Err(ResolverError::Conflict);
        }
        if !status.is_success() {
            let body_text = response.text().unwrap_or_default();
            return Err(ResolverError::Status(code, body_text));
        }

        let gen_resp: PhashGenerationResponse = response.json()?;
        Ok(gen_resp)
    }

    pub fn get_phash_job(
        &self,
        base_url: &str,
        token: &str,
        job_id: &str,
    ) -> Result<PhashJobStatusResponse> {
        let url = format!(
            "{}/admin/api/v2/video-phash/jobs/{}",
            base_url.trim_end_matches('/'),
            crate::urlencoding(job_id.trim())
        );
        let response = self.client.get(&url).header("X-Emby-Token", token).send()?;

        let status = response.status();
        let code = status.as_u16();

        if code == 401 || code == 403 {
            return Err(ResolverError::Auth(code));
        }
        if code == 404 {
            return Err(ResolverError::NotFound);
        }
        if code == 409 {
            return Err(ResolverError::Conflict);
        }
        if !status.is_success() {
            let body_text = response.text().unwrap_or_default();
            return Err(ResolverError::Status(code, body_text));
        }

        let job_resp: PhashJobStatusResponse = response.json()?;
        Ok(job_resp)
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

    #[test]
    fn parses_phash_generation_response() {
        let json_queued = r#"{
            "ok": true,
            "job_id": "job_12345",
            "state": "queued",
            "already_computed": false,
            "thumbnail_ready": false
        }"#;
        let resp: PhashGenerationResponse = serde_json::from_str(json_queued).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.job_id.as_deref(), Some("job_12345"));
        assert_eq!(resp.state.as_deref(), Some("queued"));
        assert!(!resp.already_computed);

        let json_done = r#"{
            "ok": true,
            "job_id": null,
            "state": "done",
            "already_computed": true
        }"#;
        let resp_done: PhashGenerationResponse = serde_json::from_str(json_done).unwrap();
        assert!(resp_done.ok);
        assert!(resp_done.already_computed);
    }

    #[test]
    fn parses_phash_job_status_response() {
        let json_status = r#"{
            "ok": true,
            "job": {
                "id": "job_12345",
                "video_id": "telegram:-100123:456:789",
                "state": "processing",
                "decoder_profile": "resolver-ffmpeg-stash-v2",
                "thumbnail_ready": false
            }
        }"#;
        let resp: PhashJobStatusResponse = serde_json::from_str(json_status).unwrap();
        assert!(resp.ok);
        let job = resp.job.expect("job exists");
        assert_eq!(job.id, "job_12345");
        assert_eq!(job.video_id, "telegram:-100123:456:789");
        assert_eq!(job.state, "processing");
        assert_eq!(
            job.decoder_profile.as_deref(),
            Some("resolver-ffmpeg-stash-v2")
        );
    }

    #[test]
    fn resolver_error_retryability() {
        assert!(ResolverError::Status(429, "Too Many Requests".into()).is_retryable());
        assert!(ResolverError::Status(500, "Internal Server Error".into()).is_retryable());
        assert!(ResolverError::Status(503, "Service Unavailable".into()).is_retryable());
        assert!(!ResolverError::Status(400, "Bad Request".into()).is_retryable());
        assert!(!ResolverError::Auth(401).is_retryable());
        assert!(!ResolverError::Auth(403).is_retryable());
        assert!(!ResolverError::NotFound.is_retryable());
        assert!(!ResolverError::Conflict.is_retryable());
    }
}
