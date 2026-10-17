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
                    PRIMARY KEY (channel_id, video_id)
                );

                INSERT INTO schema_version (version) VALUES (1);
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
}
