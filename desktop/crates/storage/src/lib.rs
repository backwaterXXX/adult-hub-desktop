use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Mutex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("store error: {0}")]
    Custom(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TelegramProgressRecord {
    pub provider_video_id: String,
    pub channel_id: Option<String>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub title: String,
    pub thumbnail_url: Option<String>,
    pub is_played: bool,
    pub updated_at: u64,
}

impl TelegramProgressRecord {
    pub fn progress_fraction(&self) -> f32 {
        if self.duration_ms > 0 {
            (self.position_ms as f32 / self.duration_ms as f32).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoredPhashJob {
    pub resolver_scope: String,
    pub video_id: String,
    pub job_id: Option<String>,
    pub state: String,
    pub error_code: Option<String>,
    pub duration_us: Option<u64>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfigExport {
    #[serde(default = "default_config_version")]
    pub version: i32,
    #[serde(default)]
    pub selected_library_id: Option<String>,
    #[serde(default)]
    pub selected_library_name: Option<String>,
    #[serde(default)]
    pub resolver_base_url: Option<String>,
    #[serde(default = "default_cache_limit_mb")]
    pub cache_limit_mb: i32,
    #[serde(default)]
    pub phash_on_playback: bool,
    #[serde(default = "default_buffer_profile")]
    pub buffer_profile: String,
    #[serde(default)]
    pub low_ram_mode: bool,
    #[serde(default = "default_seek_step_sec")]
    pub seek_step_sec: i32,
    #[serde(default)]
    pub autoplay_next: bool,
    #[serde(default)]
    pub selected_home_category_ids: Option<Vec<String>>,
    #[serde(default)]
    pub hidden_channel_ids: Vec<String>,
    #[serde(default)]
    pub excluded_tags: Vec<String>,
    #[serde(default = "default_auto_update_enabled")]
    pub auto_update_enabled: bool,
}

fn default_config_version() -> i32 {
    1
}
fn default_cache_limit_mb() -> i32 {
    256
}
fn default_buffer_profile() -> String {
    "standard".into()
}
fn default_seek_step_sec() -> i32 {
    15
}
fn default_auto_update_enabled() -> bool {
    true
}

impl Default for AppConfigExport {
    fn default() -> Self {
        Self {
            version: default_config_version(),
            selected_library_id: None,
            selected_library_name: None,
            resolver_base_url: None,
            cache_limit_mb: default_cache_limit_mb(),
            phash_on_playback: false,
            buffer_profile: default_buffer_profile(),
            low_ram_mode: false,
            seek_step_sec: default_seek_step_sec(),
            autoplay_next: false,
            selected_home_category_ids: None,
            hidden_channel_ids: Vec::new(),
            excluded_tags: Vec::new(),
            auto_update_enabled: default_auto_update_enabled(),
        }
    }
}

pub struct LocalStore {
    conn: Mutex<Connection>,
}

impl LocalStore {
    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| StorageError::Custom("database mutex poisoned".into()))
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.run_migrations()?;
        Ok(store)
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY
            );
            ",
        )?;

        let current_version: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if current_version < 1 {
            conn.execute_batch(
                "
                BEGIN;
                CREATE TABLE IF NOT EXISTS settings (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS search_history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    query TEXT UNIQUE NOT NULL,
                    created_at INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS local_progress (
                    channel_id TEXT NOT NULL,
                    video_id TEXT NOT NULL,
                    position_ms INTEGER NOT NULL,
                    duration_ms INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    is_played INTEGER NOT NULL DEFAULT 0,
                    title TEXT NOT NULL DEFAULT '',
                    thumbnail_url TEXT,
                    PRIMARY KEY (channel_id, video_id)
                );

                INSERT INTO schema_version (version) VALUES (1);
                COMMIT;
                ",
            )?;
        }

        let v2_needed = current_version < 2 && conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('local_progress') WHERE name = 'is_played'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count == 0)
            .unwrap_or(false);

        if v2_needed {
            conn.execute_batch(
                "
                BEGIN;
                ALTER TABLE local_progress ADD COLUMN is_played INTEGER NOT NULL DEFAULT 0;
                ALTER TABLE local_progress ADD COLUMN title TEXT NOT NULL DEFAULT '';
                ALTER TABLE local_progress ADD COLUMN thumbnail_url TEXT;
                INSERT INTO schema_version (version) VALUES (2);
                COMMIT;
                ",
            )?;
        }

        let v3_needed = current_version < 3
            && conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='phash_jobs'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count == 0)
                .unwrap_or(false);

        if v3_needed {
            conn.execute_batch(
                "
                BEGIN;
                CREATE TABLE IF NOT EXISTS phash_jobs (
                    video_id TEXT PRIMARY KEY,
                    job_id TEXT,
                    state TEXT NOT NULL,
                    error_code TEXT,
                    duration_us INTEGER,
                    updated_at INTEGER NOT NULL
                );
                INSERT INTO schema_version (version) VALUES (3);
                COMMIT;
                ",
            )?;
        }

        if current_version < 4 {
            conn.execute_batch(
                "
                BEGIN;
                CREATE TABLE IF NOT EXISTS watch_history (
                    item_id TEXT PRIMARY KEY,
                    watched_at INTEGER NOT NULL,
                    item_json TEXT NOT NULL
                );
                INSERT INTO schema_version (version) VALUES (4);
                COMMIT;
                ",
            )?;
        }

        if current_version < 5 {
            conn.execute_batch(
                "
                BEGIN;
                ALTER TABLE phash_jobs RENAME TO phash_jobs_v4;
                CREATE TABLE phash_jobs (
                    resolver_scope TEXT NOT NULL,
                    video_id TEXT NOT NULL,
                    job_id TEXT,
                    state TEXT NOT NULL,
                    error_code TEXT,
                    duration_us INTEGER,
                    updated_at INTEGER NOT NULL,
                    PRIMARY KEY (resolver_scope, video_id)
                );
                INSERT INTO phash_jobs (resolver_scope, video_id, job_id, state, error_code, duration_us, updated_at)
                    SELECT '', video_id, job_id, state, error_code, duration_us, updated_at FROM phash_jobs_v4;
                DROP TABLE phash_jobs_v4;
                INSERT INTO schema_version (version) VALUES (5);
                COMMIT;
                ",
            )?;
        }

        if current_version < 6 {
            conn.execute_batch(
                "
                BEGIN;
                ALTER TABLE search_history RENAME TO search_history_v5;
                CREATE TABLE search_history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    context TEXT NOT NULL DEFAULT 'movies',
                    query TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    UNIQUE(context, query)
                );
                INSERT INTO search_history (context, query, created_at)
                    SELECT 'movies', query, created_at FROM search_history_v5;
                DROP TABLE search_history_v5;
                CREATE INDEX IF NOT EXISTS idx_search_history_ctx_date
                    ON search_history (context, created_at DESC);
                INSERT INTO schema_version (version) VALUES (6);
                COMMIT;
                ",
            )?;
        }

        Ok(())
    }

    // Settings helpers
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn remove_setting(&self, key: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }

    pub fn approved_cleartext_origins(&self) -> Result<Vec<String>> {
        let Some(raw) = self.get_setting("approved_cleartext_origins")? else {
            return Ok(Vec::new());
        };
        let mut origins = serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default();
        origins.sort();
        origins.dedup();
        Ok(origins)
    }

    pub fn approve_cleartext_origin(&self, origin: &str) -> Result<()> {
        let origin = origin.trim().trim_end_matches('/');
        if origin.is_empty() {
            return Err(StorageError::Custom("HTTP origin cannot be empty".into()));
        }
        let mut origins = self.approved_cleartext_origins()?;
        if !origins.iter().any(|existing| existing == origin) {
            origins.push(origin.to_string());
            origins.sort();
        }
        self.set_setting(
            "approved_cleartext_origins",
            &serde_json::to_string(&origins)?,
        )
    }

    pub fn revoke_cleartext_origin(&self, origin: &str) -> Result<()> {
        let mut origins = self.approved_cleartext_origins()?;
        origins.retain(|existing| existing != origin);
        self.set_setting(
            "approved_cleartext_origins",
            &serde_json::to_string(&origins)?,
        )
    }

    // Search history helpers
    pub fn add_search_query(&self, query: &str) -> Result<()> {
        self.add_search_query_context("movies", query)
    }

    pub fn add_search_query_context(&self, context: &str, query: &str) -> Result<()> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let context = if context.trim().is_empty() {
            "movies"
        } else {
            context.trim()
        };

        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO search_history (context, query, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(context, query) DO UPDATE SET created_at = ?3",
            params![context, trimmed, now],
        )?;
        Ok(())
    }

    pub fn get_search_history(&self, limit: usize) -> Result<Vec<String>> {
        self.get_search_history_context("movies", limit)
    }

    pub fn get_search_history_context(&self, context: &str, limit: usize) -> Result<Vec<String>> {
        let context = if context.trim().is_empty() {
            "movies"
        } else {
            context.trim()
        };
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT query FROM search_history WHERE context = ?1 ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![context, limit as i64], |row| row.get(0))?;
        let mut results = Vec::new();
        for r in rows {
            results.push(r?);
        }
        Ok(results)
    }

    pub fn clear_search_history(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM search_history", [])?;
        Ok(())
    }

    pub fn clear_search_history_context(&self, context: &str) -> Result<()> {
        let context = if context.trim().is_empty() {
            "movies"
        } else {
            context.trim()
        };
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM search_history WHERE context = ?1",
            params![context],
        )?;
        Ok(())
    }

    // Local progress helpers
    pub fn save_progress(
        &self,
        channel_id: &str,
        video_id: &str,
        position_ms: u64,
        duration_ms: u64,
    ) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO local_progress (channel_id, video_id, position_ms, duration_ms, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(channel_id, video_id) DO UPDATE SET
                position_ms = ?3,
                duration_ms = ?4,
                updated_at = ?5",
            params![channel_id, video_id, position_ms as i64, duration_ms as i64, now],
        )?;
        Ok(())
    }

    pub fn get_progress(&self, channel_id: &str, video_id: &str) -> Result<Option<(u64, u64)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT position_ms, duration_ms FROM local_progress WHERE channel_id = ?1 AND video_id = ?2",
        )?;
        let mut rows = stmt.query(params![channel_id, video_id])?;
        if let Some(row) = rows.next()? {
            let pos: i64 = row.get(0)?;
            let dur: i64 = row.get(1)?;
            Ok(Some((pos.max(0) as u64, dur.max(0) as u64)))
        } else {
            Ok(None)
        }
    }

    pub fn save_telegram_progress(&self, record: &TelegramProgressRecord) -> Result<()> {
        let channel = record.channel_id.as_deref().unwrap_or("telegram");
        let is_played = if record.is_played { 1 } else { 0 };
        let now = if record.updated_at > 0 {
            record.updated_at as i64
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64
        };

        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO local_progress (channel_id, video_id, position_ms, duration_ms, updated_at, is_played, title, thumbnail_url)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(channel_id, video_id) DO UPDATE SET
                position_ms = ?3,
                duration_ms = ?4,
                updated_at = ?5,
                is_played = ?6,
                title = CASE WHEN ?7 != '' THEN ?7 ELSE title END,
                thumbnail_url = COALESCE(?8, thumbnail_url)",
            params![
                channel,
                record.provider_video_id,
                record.position_ms as i64,
                record.duration_ms as i64,
                now,
                is_played,
                record.title,
                record.thumbnail_url,
            ],
        )?;
        Ok(())
    }

    pub fn get_telegram_progress(
        &self,
        provider_video_id: &str,
    ) -> Result<Option<TelegramProgressRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT channel_id, video_id, position_ms, duration_ms, updated_at, is_played, title, thumbnail_url
             FROM local_progress
             WHERE video_id = ?1
             ORDER BY updated_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![provider_video_id])?;
        if let Some(row) = rows.next()? {
            let channel_id: String = row.get(0)?;
            let video_id: String = row.get(1)?;
            let pos: i64 = row.get(2)?;
            let dur: i64 = row.get(3)?;
            let updated: i64 = row.get(4)?;
            let is_played: i32 = row.get(5)?;
            let title: String = row.get(6)?;
            let thumb: Option<String> = row.get(7)?;

            Ok(Some(TelegramProgressRecord {
                provider_video_id: video_id,
                channel_id: Some(channel_id),
                position_ms: pos.max(0) as u64,
                duration_ms: dur.max(0) as u64,
                title,
                thumbnail_url: thumb,
                is_played: is_played != 0,
                updated_at: updated.max(0) as u64,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn mark_telegram_played(
        &self,
        provider_video_id: &str,
        is_played: bool,
        title: Option<&str>,
        thumbnail_url: Option<&str>,
        channel_id: Option<&str>,
    ) -> Result<()> {
        let existing = self.get_telegram_progress(provider_video_id)?;
        let channel = channel_id
            .map(str::to_string)
            .or_else(|| existing.as_ref().and_then(|e| e.channel_id.clone()))
            .unwrap_or_else(|| "telegram".into());
        let pos = if is_played {
            0
        } else {
            existing.as_ref().map(|e| e.position_ms).unwrap_or(0)
        };
        let dur = existing.as_ref().map(|e| e.duration_ms).unwrap_or(0);
        let title_str = title
            .map(str::to_string)
            .or_else(|| existing.as_ref().map(|e| e.title.clone()))
            .unwrap_or_else(|| "Telegram Video".into());
        let thumb_str = thumbnail_url
            .map(str::to_string)
            .or_else(|| existing.as_ref().and_then(|e| e.thumbnail_url.clone()));

        let record = TelegramProgressRecord {
            provider_video_id: provider_video_id.into(),
            channel_id: Some(channel),
            position_ms: pos,
            duration_ms: dur,
            title: title_str,
            thumbnail_url: thumb_str,
            is_played,
            updated_at: 0,
        };
        self.save_telegram_progress(&record)
    }

    pub fn get_all_telegram_progress(&self, limit: usize) -> Result<Vec<TelegramProgressRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT channel_id, video_id, position_ms, duration_ms, updated_at, is_played, title, thumbnail_url
             FROM local_progress
             ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let channel_id: String = row.get(0)?;
            let video_id: String = row.get(1)?;
            let pos: i64 = row.get(2)?;
            let dur: i64 = row.get(3)?;
            let updated: i64 = row.get(4)?;
            let is_played: i32 = row.get(5)?;
            let title: String = row.get(6)?;
            let thumb: Option<String> = row.get(7)?;

            Ok(TelegramProgressRecord {
                provider_video_id: video_id,
                channel_id: Some(channel_id),
                position_ms: pos.max(0) as u64,
                duration_ms: dur.max(0) as u64,
                title,
                thumbnail_url: thumb,
                is_played: is_played != 0,
                updated_at: updated.max(0) as u64,
            })
        })?;

        let mut results = Vec::new();
        for r in rows {
            results.push(r?);
        }
        Ok(results)
    }

    pub fn clear_telegram_progress(&self, provider_video_id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM local_progress WHERE video_id = ?1",
            params![provider_video_id],
        )?;
        Ok(())
    }

    pub fn clear_all_progress(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM local_progress", [])?;
        Ok(())
    }

    pub fn clear_played_history(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(
            "DELETE FROM watch_history; DELETE FROM local_progress WHERE is_played = 1;",
        )?;
        Ok(())
    }

    pub fn record_watch_history(&self, item_id: &str, item_json: &str) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO watch_history (item_id, watched_at, item_json) VALUES (?1, ?2, ?3)
             ON CONFLICT(item_id) DO UPDATE SET watched_at = ?2, item_json = ?3",
            params![item_id, now, item_json],
        )?;
        conn.execute(
            "DELETE FROM watch_history WHERE item_id IN (
                SELECT item_id FROM watch_history ORDER BY watched_at DESC LIMIT -1 OFFSET 100
            )",
            [],
        )?;
        Ok(())
    }

    pub fn get_watch_history(&self, limit: usize) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT item_json FROM watch_history ORDER BY watched_at DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], |row| row.get(0))?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    // Read only for one-time migration from releases that stored session
    // tokens in SQLite. New sessions are persisted by CredentialStore.
    pub fn load_legacy_session(&self) -> Result<Option<Session>> {
        let Some(server_url) = self.get_setting("session_server_url")? else {
            return Ok(None);
        };
        let Some(user_id) = self.get_setting("session_user_id")? else {
            return Ok(None);
        };
        let username = self.get_setting("session_username")?.unwrap_or_default();
        let Some(access_token) = self.get_setting("session_access_token")? else {
            return Ok(None);
        };
        let server_name = self.get_setting("session_server_name")?.unwrap_or_default();
        if server_url.trim().is_empty() || access_token.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(Session {
            server_url,
            user_id,
            username,
            access_token,
            server_name,
        }))
    }

    pub fn clear_session(&self) -> Result<()> {
        self.remove_setting("session_server_url")?;
        self.remove_setting("session_user_id")?;
        self.remove_setting("session_username")?;
        self.remove_setting("session_access_token")?;
        self.remove_setting("session_server_name")?;
        Ok(())
    }

    pub fn get_or_create_device_id(&self) -> Result<String> {
        if let Some(id) = self.get_setting("device_id")? {
            if !id.trim().is_empty() {
                return Ok(id);
            }
        }
        let new_id = uuid::Uuid::new_v4().to_string();
        self.set_setting("device_id", &new_id)?;
        Ok(new_id)
    }

    // pHash jobs are scoped by normalized Resolver origin and account.
    pub fn save_phash_job(&self, job: &StoredPhashJob) -> Result<()> {
        let now = if job.updated_at > 0 {
            job.updated_at as i64
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64
        };

        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO phash_jobs (resolver_scope, video_id, job_id, state, error_code, duration_us, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(resolver_scope, video_id) DO UPDATE SET
                job_id = ?3,
                state = ?4,
                error_code = ?5,
                duration_us = ?6,
                updated_at = ?7",
            params![
                job.resolver_scope,
                job.video_id,
                job.job_id,
                job.state,
                job.error_code,
                job.duration_us.map(|d| d as i64),
                now,
            ],
        )?;

        // Trim to MAX_JOBS (200), matching Android PhashJobStore
        conn.execute(
            "DELETE FROM phash_jobs WHERE (resolver_scope, video_id) NOT IN (
                SELECT resolver_scope, video_id FROM phash_jobs ORDER BY updated_at DESC LIMIT 200
            )",
            [],
        )?;
        Ok(())
    }

    pub fn get_phash_job(
        &self,
        resolver_scope: &str,
        video_id: &str,
    ) -> Result<Option<StoredPhashJob>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT resolver_scope, video_id, job_id, state, error_code, duration_us, updated_at
             FROM phash_jobs WHERE resolver_scope = ?1 AND video_id = ?2",
        )?;
        let mut rows = stmt.query(params![resolver_scope, video_id])?;
        if let Some(row) = rows.next()? {
            let resolver_scope: String = row.get(0)?;
            let video_id: String = row.get(1)?;
            let job_id: Option<String> = row.get(2)?;
            let state: String = row.get(3)?;
            let error_code: Option<String> = row.get(4)?;
            let dur: Option<i64> = row.get(5)?;
            let updated: i64 = row.get(6)?;
            Ok(Some(StoredPhashJob {
                resolver_scope,
                video_id,
                job_id,
                state,
                error_code,
                duration_us: dur.map(|d| d.max(0) as u64),
                updated_at: updated.max(0) as u64,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_phash_jobs_for_scope(&self, resolver_scope: &str) -> Result<Vec<StoredPhashJob>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT resolver_scope, video_id, job_id, state, error_code, duration_us, updated_at
             FROM phash_jobs WHERE resolver_scope = ?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(params![resolver_scope], |row| {
            let resolver_scope: String = row.get(0)?;
            let video_id: String = row.get(1)?;
            let job_id: Option<String> = row.get(2)?;
            let state: String = row.get(3)?;
            let error_code: Option<String> = row.get(4)?;
            let dur: Option<i64> = row.get(5)?;
            let updated: i64 = row.get(6)?;
            Ok(StoredPhashJob {
                resolver_scope,
                video_id,
                job_id,
                state,
                error_code,
                duration_us: dur.map(|d| d.max(0) as u64),
                updated_at: updated.max(0) as u64,
            })
        })?;
        let mut results = Vec::new();
        for r in rows {
            results.push(r?);
        }
        Ok(results)
    }

    pub fn remove_phash_job(&self, resolver_scope: &str, video_id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM phash_jobs WHERE resolver_scope = ?1 AND video_id = ?2",
            params![resolver_scope, video_id],
        )?;
        Ok(())
    }

    pub fn clear_all_phash_jobs(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM phash_jobs", [])?;
        Ok(())
    }

    // Config export and import (Version 1 Schema)
    pub fn export_non_secret_config(&self) -> Result<AppConfigExport> {
        let selected_library_id = self.get_setting("selected_library_id")?;
        let selected_library_name = self.get_setting("selected_library_name")?;
        let resolver_base_url = self
            .get_setting("resolver_base_url")?
            .or_else(|| self.get_setting("resolver_url").ok().flatten());
        let cache_limit_mb = self
            .get_setting("cache_limit_mb")?
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(256);
        let phash_on_playback = self
            .get_setting("phash_on_playback")?
            .map(|s| s == "true" || s == "1")
            .unwrap_or(false);
        let buffer_profile = self
            .get_setting("buffer_profile")?
            .unwrap_or_else(|| "standard".into());
        let low_ram_mode = self
            .get_setting("low_ram_mode")?
            .map(|s| s == "true" || s == "1")
            .unwrap_or(false);
        let seek_step_sec = self
            .get_setting("seek_step_sec")?
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(15);
        let autoplay_next = self
            .get_setting("autoplay_next")?
            .map(|s| s == "true" || s == "1")
            .unwrap_or(false);
        let selected_home_category_ids = self.get_home_categories()?;
        let hidden_channel_ids = self.get_hidden_channels()?;
        let excluded_tags = self.get_excluded_tags()?;
        let auto_update_enabled = self
            .get_setting("auto_update_enabled")?
            .map(|s| s == "true" || s == "1")
            .unwrap_or(true);

        Ok(AppConfigExport {
            version: 1,
            selected_library_id,
            selected_library_name,
            resolver_base_url,
            cache_limit_mb,
            phash_on_playback,
            buffer_profile,
            low_ram_mode,
            seek_step_sec,
            autoplay_next,
            selected_home_category_ids,
            hidden_channel_ids,
            excluded_tags,
            auto_update_enabled,
        })
    }

    pub fn export_non_secret_config_json(&self) -> Result<String> {
        let config = self.export_non_secret_config()?;
        Ok(serde_json::to_string_pretty(&config)?)
    }

    pub fn import_non_secret_config(&self, config: &AppConfigExport) -> Result<()> {
        // Reject unsupported versions without partial application
        if config.version != 1 {
            return Err(StorageError::Custom(format!(
                "Unsupported configuration version: {}",
                config.version
            )));
        }
        if config.cache_limit_mb < 0 {
            return Err(StorageError::Custom(
                "cacheLimitMb must be non-negative".into(),
            ));
        }
        if config.seek_step_sec <= 0 {
            return Err(StorageError::Custom("seekStepSec must be positive".into()));
        }
        let profile = config.buffer_profile.as_str();
        if profile != "fast_start" && profile != "standard" && profile != "large" {
            return Err(StorageError::Custom(format!(
                "Unsupported buffer profile: {}",
                config.buffer_profile
            )));
        }

        // Apply all-or-nothing in a single transaction
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;

        if let (Some(lib_id), Some(lib_name)) =
            (&config.selected_library_id, &config.selected_library_name)
        {
            tx.execute(
                "INSERT INTO settings (key, value) VALUES ('selected_library_id', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = ?1",
                params![lib_id],
            )?;
            tx.execute(
                "INSERT INTO settings (key, value) VALUES ('selected_library_name', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = ?1",
                params![lib_name],
            )?;
        }
        if let Some(resolver_url) = &config.resolver_base_url {
            let sanitized = resolver_url.trim_end_matches('/');
            if sanitized.is_empty() {
                tx.execute("DELETE FROM settings WHERE key = 'resolver_base_url'", [])?;
                tx.execute("DELETE FROM settings WHERE key = 'resolver_url'", [])?;
            } else {
                tx.execute(
                    "INSERT INTO settings (key, value) VALUES ('resolver_base_url', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = ?1",
                    params![sanitized],
                )?;
                tx.execute(
                    "INSERT INTO settings (key, value) VALUES ('resolver_url', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = ?1",
                    params![sanitized],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('cache_limit_mb', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            params![config.cache_limit_mb.to_string()],
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('phash_on_playback', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            params![if config.phash_on_playback {
                "true"
            } else {
                "false"
            }],
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('buffer_profile', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            params![config.buffer_profile],
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('low_ram_mode', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            params![if config.low_ram_mode { "true" } else { "false" }],
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('seek_step_sec', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            params![config.seek_step_sec.to_string()],
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('autoplay_next', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            params![if config.autoplay_next {
                "true"
            } else {
                "false"
            }],
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('auto_update_enabled', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            params![if config.auto_update_enabled {
                "true"
            } else {
                "false"
            }],
        )?;

        if let Some(cat_ids) = &config.selected_home_category_ids {
            let json_cats = serde_json::to_string(cat_ids)?;
            tx.execute(
                "INSERT INTO settings (key, value) VALUES ('selected_home_category_ids', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = ?1",
                params![json_cats],
            )?;
        }

        let json_hidden = serde_json::to_string(&config.hidden_channel_ids)?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('hidden_channel_ids', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            params![json_hidden],
        )?;

        let json_tags = serde_json::to_string(&config.excluded_tags)?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('excluded_tags', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            params![json_tags],
        )?;

        tx.commit()?;
        Ok(())
    }

    pub fn import_non_secret_config_json(&self, json_str: &str) -> Result<()> {
        let config: AppConfigExport = serde_json::from_str(json_str)
            .map_err(|e| StorageError::Custom(format!("Invalid configuration JSON: {e}")))?;
        self.import_non_secret_config(&config)
    }

    // Hidden channels
    pub fn get_hidden_channels(&self) -> Result<Vec<String>> {
        if let Some(val) = self.get_setting("hidden_channel_ids")? {
            let ids: Vec<String> = serde_json::from_str(&val).unwrap_or_default();
            Ok(ids)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn hide_channel(&self, channel_id: &str) -> Result<()> {
        let mut ids = self.get_hidden_channels()?;
        if !ids.iter().any(|i| i == channel_id) {
            ids.push(channel_id.to_string());
            let json = serde_json::to_string(&ids)?;
            self.set_setting("hidden_channel_ids", &json)?;
        }
        Ok(())
    }

    pub fn unhide_channel(&self, channel_id: &str) -> Result<()> {
        let mut ids = self.get_hidden_channels()?;
        ids.retain(|i| i != channel_id);
        let json = serde_json::to_string(&ids)?;
        self.set_setting("hidden_channel_ids", &json)?;
        Ok(())
    }

    pub fn clear_hidden_channels(&self) -> Result<()> {
        let empty: Vec<String> = Vec::new();
        let json = serde_json::to_string(&empty)?;
        self.set_setting("hidden_channel_ids", &json)?;
        Ok(())
    }

    // Selected channel persistence (only for single channels, never "All Channels")
    pub fn get_selected_channel(&self) -> Result<Option<String>> {
        let val = self.get_setting("selected_single_channel")?;
        Ok(val.filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("all channels")))
    }

    pub fn set_selected_channel(&self, channel: Option<&str>) -> Result<()> {
        match channel {
            Some(c) if !c.trim().is_empty() && !c.eq_ignore_ascii_case("all channels") => {
                self.set_setting("selected_single_channel", c.trim())?;
            }
            _ => {
                self.remove_setting("selected_single_channel")?;
            }
        }
        Ok(())
    }

    // Excluded tags
    pub fn get_excluded_tags(&self) -> Result<Vec<String>> {
        if let Some(val) = self.get_setting("excluded_tags")? {
            let tags: Vec<String> = serde_json::from_str(&val).unwrap_or_default();
            Ok(tags)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn exclude_tag(&self, tag: &str) -> Result<()> {
        let trimmed = tag.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let mut tags = self.get_excluded_tags()?;
        if !tags.iter().any(|t| t.eq_ignore_ascii_case(trimmed)) {
            tags.push(trimmed.to_string());
            let json = serde_json::to_string(&tags)?;
            self.set_setting("excluded_tags", &json)?;
        }
        Ok(())
    }

    pub fn remove_excluded_tag(&self, tag: &str) -> Result<()> {
        let mut tags = self.get_excluded_tags()?;
        tags.retain(|t| !t.eq_ignore_ascii_case(tag.trim()));
        let json = serde_json::to_string(&tags)?;
        self.set_setting("excluded_tags", &json)?;
        Ok(())
    }

    // Home categories
    pub fn get_home_categories(&self) -> Result<Option<Vec<String>>> {
        if let Some(val) = self.get_setting("selected_home_category_ids")? {
            let ids: Vec<String> = serde_json::from_str(&val).unwrap_or_default();
            Ok(Some(ids))
        } else {
            Ok(None)
        }
    }

    pub fn set_home_categories(&self, ids: Option<&[String]>) -> Result<()> {
        if let Some(ids) = ids {
            let json = serde_json::to_string(ids)?;
            self.set_setting("selected_home_category_ids", &json)?;
        } else {
            self.remove_setting("selected_home_category_ids")?;
        }
        Ok(())
    }
}

pub fn default_db_path() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return std::path::PathBuf::from(appdata)
                .join("AdultHubDesktop")
                .join("store.db");
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("adult-hub-desktop")
                .join("store.db");
        }
    }
    std::path::PathBuf::from("adult_hub.db")
}

/// CredentialStore handles active session tokens and user identity.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub server_url: String,
    pub user_id: String,
    pub username: String,
    pub access_token: String,
    pub server_name: String,
}

pub struct CredentialStore {
    in_memory: std::sync::RwLock<Option<Session>>,
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialStore {
    pub fn new() -> Self {
        Self {
            in_memory: std::sync::RwLock::new(load_platform_session()),
        }
    }

    pub fn save(&self, session: Session) -> Result<()> {
        let mut lock = self
            .in_memory
            .write()
            .map_err(|_| StorageError::Custom("lock error".into()))?;
        *lock = Some(session.clone());
        persist_platform_session(&session)
    }

    pub fn get(&self) -> Result<Option<Session>> {
        let lock = self
            .in_memory
            .read()
            .map_err(|_| StorageError::Custom("lock error".into()))?;
        if lock.is_some() {
            return Ok(lock.clone());
        }
        drop(lock);
        let loaded = load_platform_session();
        if loaded.is_some() {
            *self
                .in_memory
                .write()
                .map_err(|_| StorageError::Custom("lock error".into()))? = loaded.clone();
        }
        Ok(loaded)
    }

    pub fn clear(&self) -> Result<()> {
        let mut lock = self
            .in_memory
            .write()
            .map_err(|_| StorageError::Custom("lock error".into()))?;
        *lock = None;
        clear_platform_session()
    }
}

#[cfg(windows)]
fn load_platform_session() -> Option<Session> {
    use std::ffi::c_void;
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };

    let target: Vec<u16> = "AdultHubDesktop/session"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if ok == 0 || credential.is_null() {
        return None;
    }
    let value = unsafe {
        let blob = (*credential).CredentialBlob;
        let len = (*credential).CredentialBlobSize as usize;
        if blob.is_null() || len == 0 {
            None
        } else {
            serde_json::from_slice(std::slice::from_raw_parts(blob, len)).ok()
        }
    };
    unsafe { CredFree(credential.cast::<c_void>()) };
    value
}

#[cfg(not(windows))]
fn load_platform_session() -> Option<Session> {
    None
}

#[cfg(windows)]
fn persist_platform_session(session: &Session) -> Result<()> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredWriteW,
    };

    let serialized = serde_json::to_vec(session)?;
    let blob_size = u32::try_from(serialized.len())
        .map_err(|_| StorageError::Custom("session data is too large".into()))?;
    let target: Vec<u16> = "AdultHubDesktop/session"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let username: Vec<u16> = session.username.encode_utf16().chain(Some(0)).collect();
    let credential = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_ptr().cast_mut(),
        Comment: std::ptr::null_mut(),
        LastWritten: FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        CredentialBlobSize: blob_size,
        CredentialBlob: serialized.as_ptr().cast_mut(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: std::ptr::null_mut(),
        UserName: username.as_ptr().cast_mut(),
    };
    let ok = unsafe { CredWriteW(&credential, 0) };
    if ok == 0 {
        return Err(StorageError::Custom(format!(
            "Windows Credential Manager rejected the session (error {})",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn persist_platform_session(_session: &Session) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn clear_platform_session() -> Result<()> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW};

    let target: Vec<u16> = "AdultHubDesktop/session"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        if error != 1168 {
            return Err(StorageError::Custom(format!(
                "cannot clear saved session (Windows error {error})"
            )));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn clear_platform_session() -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_crud() {
        let store = LocalStore::open_in_memory().unwrap();
        assert_eq!(store.get_setting("server_url").unwrap(), None);

        store
            .set_setting("server_url", "http://localhost:8096")
            .unwrap();
        assert_eq!(
            store.get_setting("server_url").unwrap().as_deref(),
            Some("http://localhost:8096")
        );

        store
            .set_setting("server_url", "https://example.com")
            .unwrap();
        assert_eq!(
            store.get_setting("server_url").unwrap().as_deref(),
            Some("https://example.com")
        );

        store.remove_setting("server_url").unwrap();
        assert_eq!(store.get_setting("server_url").unwrap(), None);
    }

    #[test]
    fn search_history_ordering() {
        let store = LocalStore::open_in_memory().unwrap();
        store.add_search_query("query 1").unwrap();
        store.add_search_query("query 2").unwrap();
        store.add_search_query("query 1").unwrap(); // should bump to top

        let history = store.get_search_history(10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0], "query 1");
        assert_eq!(history[1], "query 2");

        store.clear_search_history().unwrap();
        assert!(store.get_search_history(10).unwrap().is_empty());
    }

    #[test]
    fn search_history_context_segregation() {
        let store = LocalStore::open_in_memory().unwrap();
        store
            .add_search_query_context("movies", "anal blonde")
            .unwrap();
        store
            .add_search_query_context("performers", "Eva Elfie")
            .unwrap();
        store
            .add_search_query_context("studios", "Brazzers")
            .unwrap();
        store
            .add_search_query_context("channels", "Private TG")
            .unwrap();

        // Each context has only its own query
        assert_eq!(
            store.get_search_history_context("movies", 5).unwrap(),
            vec!["anal blonde"]
        );
        assert_eq!(
            store.get_search_history_context("performers", 5).unwrap(),
            vec!["Eva Elfie"]
        );
        assert_eq!(
            store.get_search_history_context("studios", 5).unwrap(),
            vec!["Brazzers"]
        );
        assert_eq!(
            store.get_search_history_context("channels", 5).unwrap(),
            vec!["Private TG"]
        );

        // Clearing one context leaves the others intact
        store.clear_search_history_context("performers").unwrap();
        assert!(
            store
                .get_search_history_context("performers", 5)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.get_search_history_context("studios", 5).unwrap(),
            vec!["Brazzers"]
        );
        assert_eq!(
            store.get_search_history_context("movies", 5).unwrap(),
            vec!["anal blonde"]
        );
    }

    #[test]
    fn progress_save_and_retrieve() {
        let store = LocalStore::open_in_memory().unwrap();
        assert_eq!(store.get_progress("ch1", "v1").unwrap(), None);

        store.save_progress("ch1", "v1", 15000, 60000).unwrap();
        let prog = store.get_progress("ch1", "v1").unwrap();
        assert_eq!(prog, Some((15000, 60000)));

        store.save_progress("ch1", "v1", 30000, 60000).unwrap();
        assert_eq!(
            store.get_progress("ch1", "v1").unwrap(),
            Some((30000, 60000))
        );
    }

    #[test]
    fn credential_store_lifecycle() {
        let store = CredentialStore::new();
        assert!(store.get().unwrap().is_none());

        let session = Session {
            server_url: "http://localhost:8096".into(),
            user_id: "u123".into(),
            username: "tester".into(),
            access_token: "tok456".into(),
            server_name: "TestServer".into(),
        };
        store.save(session.clone()).unwrap();
        let loaded = store.get().unwrap().unwrap();
        assert_eq!(loaded.username, "tester");
        assert_eq!(loaded.access_token, "tok456");

        store.clear().unwrap();
        assert!(store.get().unwrap().is_none());
    }

    #[test]
    fn local_store_session_and_device_id() {
        let store = LocalStore::open_in_memory().unwrap();
        assert!(store.load_legacy_session().unwrap().is_none());

        let session = Session {
            server_url: "https://my.server.local:8920".into(),
            user_id: "user-abc".into(),
            username: "adult_fan".into(),
            access_token: "secret-token-xyz".into(),
            server_name: "My Server".into(),
        };
        store
            .set_setting("session_server_url", &session.server_url)
            .unwrap();
        store
            .set_setting("session_user_id", &session.user_id)
            .unwrap();
        store
            .set_setting("session_username", &session.username)
            .unwrap();
        store
            .set_setting("session_access_token", &session.access_token)
            .unwrap();
        store
            .set_setting("session_server_name", &session.server_name)
            .unwrap();

        let loaded = store
            .load_legacy_session()
            .unwrap()
            .expect("legacy session loaded");
        assert_eq!(loaded.server_url, "https://my.server.local:8920");
        assert_eq!(loaded.user_id, "user-abc");
        assert_eq!(loaded.username, "adult_fan");
        assert_eq!(loaded.access_token, "secret-token-xyz");
        assert_eq!(loaded.server_name, "My Server");

        store.clear_session().unwrap();
        assert!(store.load_legacy_session().unwrap().is_none());

        let dev_id1 = store.get_or_create_device_id().unwrap();
        assert!(!dev_id1.is_empty());
        let dev_id2 = store.get_or_create_device_id().unwrap();
        assert_eq!(dev_id1, dev_id2);
    }

    #[test]
    fn telegram_progress_lifecycle() {
        let store = LocalStore::open_in_memory().unwrap();
        assert!(store.get_telegram_progress("vid_101").unwrap().is_none());

        let rec = TelegramProgressRecord {
            provider_video_id: "vid_101".into(),
            channel_id: Some("chan_tg".into()),
            position_ms: 45_000,
            duration_ms: 120_000,
            title: "Test Scene 101".into(),
            thumbnail_url: Some("http://thumb/101.jpg".into()),
            is_played: false,
            updated_at: 1000,
        };
        assert_eq!(rec.progress_fraction(), 0.375);
        store.save_telegram_progress(&rec).unwrap();

        let loaded = store
            .get_telegram_progress("vid_101")
            .unwrap()
            .expect("record found");
        assert_eq!(loaded.provider_video_id, "vid_101");
        assert_eq!(loaded.channel_id.as_deref(), Some("chan_tg"));
        assert_eq!(loaded.position_ms, 45_000);
        assert_eq!(loaded.duration_ms, 120_000);
        assert_eq!(loaded.title, "Test Scene 101");
        assert_eq!(
            loaded.thumbnail_url.as_deref(),
            Some("http://thumb/101.jpg")
        );
        assert!(!loaded.is_played);

        // Mark played
        store
            .mark_telegram_played("vid_101", true, None, None, None)
            .unwrap();
        let loaded2 = store
            .get_telegram_progress("vid_101")
            .unwrap()
            .expect("record found");
        assert!(loaded2.is_played);
        assert_eq!(loaded2.position_ms, 0);
        assert_eq!(loaded2.title, "Test Scene 101");

        let all = store.get_all_telegram_progress(10).unwrap();
        assert_eq!(all.len(), 1);

        store.clear_telegram_progress("vid_101").unwrap();
        assert!(store.get_telegram_progress("vid_101").unwrap().is_none());

        store.save_telegram_progress(&rec).unwrap();
        assert!(!store.get_all_telegram_progress(10).unwrap().is_empty());
        store.clear_all_progress().unwrap();
        assert!(store.get_all_telegram_progress(10).unwrap().is_empty());
    }

    #[test]
    fn phash_jobs_persistence() {
        let store = LocalStore::open_in_memory().unwrap();
        assert!(
            store
                .get_phash_job("resolver-a:user-a", "telegram:-1001:2:3")
                .unwrap()
                .is_none()
        );

        let job1 = StoredPhashJob {
            resolver_scope: "resolver-a:user-a".into(),
            video_id: "telegram:-1001:2:3".into(),
            job_id: Some("job_abc".into()),
            state: "queued".into(),
            error_code: None,
            duration_us: Some(120_000_000),
            updated_at: 1000,
        };
        store.save_phash_job(&job1).unwrap();

        let loaded = store
            .get_phash_job("resolver-a:user-a", "telegram:-1001:2:3")
            .unwrap()
            .expect("job loaded");
        assert_eq!(loaded.job_id.as_deref(), Some("job_abc"));
        assert_eq!(loaded.state, "queued");
        assert_eq!(loaded.duration_us, Some(120_000_000));

        // Update state to done
        let job_updated = StoredPhashJob {
            resolver_scope: "resolver-a:user-a".into(),
            video_id: "telegram:-1001:2:3".into(),
            job_id: Some("job_abc".into()),
            state: "done".into(),
            error_code: None,
            duration_us: Some(120_000_000),
            updated_at: 2000,
        };
        store.save_phash_job(&job_updated).unwrap();
        let loaded2 = store
            .get_phash_job("resolver-a:user-a", "telegram:-1001:2:3")
            .unwrap()
            .expect("job updated");
        assert_eq!(loaded2.state, "done");

        let all = store.get_phash_jobs_for_scope("resolver-a:user-a").unwrap();
        assert_eq!(all.len(), 1);

        store
            .remove_phash_job("resolver-a:user-a", "telegram:-1001:2:3")
            .unwrap();
        assert!(
            store
                .get_phash_job("resolver-a:user-a", "telegram:-1001:2:3")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_phash_jobs_for_scope("resolver-a:user-a")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn config_export_and_import_roundtrip() {
        let store = LocalStore::open_in_memory().unwrap();

        // Default export
        let default_cfg = store.export_non_secret_config().unwrap();
        assert_eq!(default_cfg.version, 1);
        assert_eq!(default_cfg.cache_limit_mb, 256);
        assert_eq!(default_cfg.seek_step_sec, 15);
        assert_eq!(default_cfg.buffer_profile, "standard");
        assert!(!default_cfg.phash_on_playback);

        // Custom config matching Android Version 1 JSON schema
        let custom_json = r#"{
            "version": 1,
            "selectedLibraryId": "lib_adult_4k",
            "selectedLibraryName": "4K Ultra Adult",
            "resolverBaseUrl": "https://resolver.example.org",
            "cacheLimitMb": 512,
            "phashOnPlayback": true,
            "bufferProfile": "fast_start",
            "lowRamMode": true,
            "seekStepSec": 30,
            "autoplayNext": true,
            "selectedHomeCategoryIds": ["cat1", "cat2"],
            "hiddenChannelIds": ["chan_hidden_9"],
            "excludedTags": ["tag_a", "tag_b"],
            "autoUpdateEnabled": false
        }"#;

        store.import_non_secret_config_json(custom_json).unwrap();

        let exported = store.export_non_secret_config().unwrap();
        assert_eq!(exported.version, 1);
        assert_eq!(
            exported.selected_library_id.as_deref(),
            Some("lib_adult_4k")
        );
        assert_eq!(
            exported.selected_library_name.as_deref(),
            Some("4K Ultra Adult")
        );
        assert_eq!(
            exported.resolver_base_url.as_deref(),
            Some("https://resolver.example.org")
        );
        assert_eq!(exported.cache_limit_mb, 512);
        assert!(exported.phash_on_playback);
        assert_eq!(exported.buffer_profile, "fast_start");
        assert!(exported.low_ram_mode);
        assert_eq!(exported.seek_step_sec, 30);
        assert!(exported.autoplay_next);
        assert_eq!(
            exported.selected_home_category_ids,
            Some(vec!["cat1".into(), "cat2".into()])
        );
        assert_eq!(exported.hidden_channel_ids, vec!["chan_hidden_9"]);
        assert_eq!(exported.excluded_tags, vec!["tag_a", "tag_b"]);
        assert!(!exported.auto_update_enabled);

        // JSON string output is valid and can be re-imported
        let json_out = store.export_non_secret_config_json().unwrap();
        let store2 = LocalStore::open_in_memory().unwrap();
        store2.import_non_secret_config_json(&json_out).unwrap();
        let exported2 = store2.export_non_secret_config().unwrap();
        assert_eq!(exported, exported2);
    }

    #[test]
    fn config_import_validation_rejects_unsupported_version_without_partial_apply() {
        let store = LocalStore::open_in_memory().unwrap();

        // Initial setup
        store.set_setting("cache_limit_mb", "100").unwrap();
        store.set_setting("seek_step_sec", "10").unwrap();

        // Attempt import with unsupported version 2
        let invalid_version_json = r#"{
            "version": 2,
            "cacheLimitMb": 9999,
            "seekStepSec": 60,
            "bufferProfile": "standard"
        }"#;

        let res = store.import_non_secret_config_json(invalid_version_json);
        assert!(res.is_err());
        assert!(
            res.unwrap_err()
                .to_string()
                .contains("Unsupported configuration version: 2")
        );

        // Settings must remain completely unmodified
        assert_eq!(
            store.get_setting("cache_limit_mb").unwrap().as_deref(),
            Some("100")
        );
        assert_eq!(
            store.get_setting("seek_step_sec").unwrap().as_deref(),
            Some("10")
        );
    }

    #[test]
    fn config_import_validation_rejects_invalid_values() {
        let store = LocalStore::open_in_memory().unwrap();
        store.set_setting("buffer_profile", "standard").unwrap();

        // Invalid buffer profile
        let bad_profile = r#"{
            "version": 1,
            "bufferProfile": "hyper_stream",
            "cacheLimitMb": 256,
            "seekStepSec": 15
        }"#;
        assert!(store.import_non_secret_config_json(bad_profile).is_err());
        assert_eq!(
            store.get_setting("buffer_profile").unwrap().as_deref(),
            Some("standard")
        );

        // Negative cache limit
        let negative_cache = r#"{
            "version": 1,
            "cacheLimitMb": -10,
            "seekStepSec": 15,
            "bufferProfile": "standard"
        }"#;
        assert!(store.import_non_secret_config_json(negative_cache).is_err());

        // Zero or negative seek step
        let zero_seek = r#"{
            "version": 1,
            "cacheLimitMb": 256,
            "seekStepSec": 0,
            "bufferProfile": "standard"
        }"#;
        assert!(store.import_non_secret_config_json(zero_seek).is_err());
    }

    #[test]
    fn hidden_channels_and_excluded_tags_lifecycle() {
        let store = LocalStore::open_in_memory().unwrap();

        // Hidden channels
        assert!(store.get_hidden_channels().unwrap().is_empty());
        store.hide_channel("chan_1").unwrap();
        store.hide_channel("chan_2").unwrap();
        store.hide_channel("chan_1").unwrap(); // deduplication
        assert_eq!(
            store.get_hidden_channels().unwrap(),
            vec!["chan_1", "chan_2"]
        );

        store.unhide_channel("chan_1").unwrap();
        assert_eq!(store.get_hidden_channels().unwrap(), vec!["chan_2"]);

        // Excluded tags
        assert!(store.get_excluded_tags().unwrap().is_empty());
        store.exclude_tag("BDSM").unwrap();
        store.exclude_tag("Anal").unwrap();
        store.exclude_tag("bdsm").unwrap(); // case-insensitive deduplication
        assert_eq!(store.get_excluded_tags().unwrap(), vec!["BDSM", "Anal"]);

        store.remove_excluded_tag("bdsm").unwrap();
        assert_eq!(store.get_excluded_tags().unwrap(), vec!["Anal"]);

        // Selected channel persistence
        assert_eq!(store.get_selected_channel().unwrap(), None);
        store.set_selected_channel(Some("THE G BUNKER")).unwrap();
        assert_eq!(store.get_selected_channel().unwrap(), Some("THE G BUNKER".into()));
        // "All Channels" must never be saved as selected single channel
        store.set_selected_channel(Some("All Channels")).unwrap();
        assert_eq!(store.get_selected_channel().unwrap(), None);
        store.set_selected_channel(Some("Channel A")).unwrap();
        assert_eq!(store.get_selected_channel().unwrap(), Some("Channel A".into()));
        store.set_selected_channel(None).unwrap();
        assert_eq!(store.get_selected_channel().unwrap(), None);
    }
}
