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

        let v2_needed: bool = conn
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

    // Search history helpers
    pub fn add_search_query(&self, query: &str) -> Result<()> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO search_history (query, created_at) VALUES (?1, ?2)
             ON CONFLICT(query) DO UPDATE SET created_at = ?2",
            params![trimmed, now],
        )?;
        Ok(())
    }

    pub fn get_search_history(&self, limit: usize) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT query FROM search_history ORDER BY created_at DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], |row| row.get(0))?;
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

    // Session persistence helpers
    pub fn save_session(&self, session: &Session) -> Result<()> {
        self.set_setting("session_server_url", &session.server_url)?;
        self.set_setting("session_user_id", &session.user_id)?;
        self.set_setting("session_username", &session.username)?;
        self.set_setting("session_access_token", &session.access_token)?;
        self.set_setting("session_server_name", &session.server_name)?;
        Ok(())
    }

    pub fn load_session(&self) -> Result<Option<Session>> {
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
#[derive(Debug, Clone, Default)]
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
            in_memory: std::sync::RwLock::new(None),
        }
    }

    pub fn save(&self, session: Session) -> Result<()> {
        let mut lock = self
            .in_memory
            .write()
            .map_err(|_| StorageError::Custom("lock error".into()))?;
        *lock = Some(session);
        Ok(())
    }

    pub fn get(&self) -> Result<Option<Session>> {
        let lock = self
            .in_memory
            .read()
            .map_err(|_| StorageError::Custom("lock error".into()))?;
        Ok(lock.clone())
    }

    pub fn clear(&self) -> Result<()> {
        let mut lock = self
            .in_memory
            .write()
            .map_err(|_| StorageError::Custom("lock error".into()))?;
        *lock = None;
        Ok(())
    }
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
        assert!(store.load_session().unwrap().is_none());

        let session = Session {
            server_url: "https://my.server.local:8920".into(),
            user_id: "user-abc".into(),
            username: "adult_fan".into(),
            access_token: "secret-token-xyz".into(),
            server_name: "My Server".into(),
        };
        store.save_session(&session).unwrap();

        let loaded = store.load_session().unwrap().expect("session loaded");
        assert_eq!(loaded.server_url, "https://my.server.local:8920");
        assert_eq!(loaded.user_id, "user-abc");
        assert_eq!(loaded.username, "adult_fan");
        assert_eq!(loaded.access_token, "secret-token-xyz");
        assert_eq!(loaded.server_name, "My Server");

        store.clear_session().unwrap();
        assert!(store.load_session().unwrap().is_none());

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
        assert_eq!(loaded.thumbnail_url.as_deref(), Some("http://thumb/101.jpg"));
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
}
