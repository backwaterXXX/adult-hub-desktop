use regex::Regex;
use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

const MAX_ENTRIES: usize = 500;
const MAX_MESSAGE_LENGTH: usize = 1_000;

static URL_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)https?://[^\s\x22'<>]+").expect("valid url regex"));

static WINDOWS_PATH_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(^|[\s'"(])([a-z]:\\[^'"<>|]+)"#).expect("valid Windows path regex")
});

static UNIX_PATH_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(^|[\s'"(])(/[^'"<>|]+)"#).expect("valid Unix path regex"));

static UUID_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\b")
        .expect("valid uuid regex")
});

static SECRET_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(token|authorization|password|api[_-]?key)\b\s*[:=]\s*[^\s,;]+")
        .expect("valid secret regex")
});

static IDENTIFIER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b((?:item|video|user|session|media[_-]?source)[_-]?id)\b\s*[:=]\s*[^\s,;]+")
        .expect("valid identifier regex")
});

static TITLE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(title|filename|file[_-]?name)\b\s*[:=]\s*[^,;\n]+")
        .expect("valid title regex")
});

pub fn redact(message: &str) -> String {
    let s1 = URL_REGEX.replace_all(message, "[URL]");
    let s2 = WINDOWS_PATH_REGEX.replace_all(&s1, "${1}[PATH]");
    let s3 = UNIX_PATH_REGEX.replace_all(&s2, "${1}[PATH]");
    let s4 = SECRET_REGEX.replace_all(&s3, "${1}=[REDACTED]");
    let s5 = IDENTIFIER_REGEX.replace_all(&s4, "${1}=[REDACTED]");
    let s6 = TITLE_REGEX.replace_all(&s5, "${1}=[REDACTED]");
    UUID_REGEX.replace_all(&s6, "[ID]").into_owned()
}

pub struct AppLog {
    entries: Mutex<VecDeque<String>>,
}

impl Default for AppLog {
    fn default() -> Self {
        Self::new()
    }
}

impl AppLog {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::with_capacity(MAX_ENTRIES)),
        }
    }

    #[allow(dead_code)]
    pub fn d(&self, tag: &str, message: &str) {
        self.write_entry("D", tag, message);
    }

    pub fn i(&self, tag: &str, message: &str) {
        self.write_entry("I", tag, message);
    }

    pub fn w(&self, tag: &str, message: &str) {
        self.write_entry("W", tag, message);
    }

    pub fn e(&self, tag: &str, message: &str) {
        self.write_entry("E", tag, message);
    }

    fn write_entry(&self, level: &str, tag: &str, message: &str) {
        let safe_msg = redact(message);
        let truncated_msg = if safe_msg.len() > MAX_MESSAGE_LENGTH {
            let mut boundary = MAX_MESSAGE_LENGTH;
            while boundary > 0 && !safe_msg.is_char_boundary(boundary) {
                boundary -= 1;
            }
            &safe_msg[..boundary]
        } else {
            &safe_msg
        };

        let now_sec = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let line = format!("{now_sec} {level} {tag}: {truncated_msg}");

        if let Ok(mut lock) = self.entries.lock() {
            if lock.len() >= MAX_ENTRIES {
                lock.pop_front();
            }
            lock.push_back(line);
        }
    }

    pub fn export_text(&self, app_version: &str, extra_info: &[(&str, &str)]) -> String {
        let entries_snapshot = if let Ok(lock) = self.entries.lock() {
            lock.iter().cloned().collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        let mut out = String::new();
        out.push_str("Adult Hub diagnostics\n");
        out.push_str(&format!("App version: {app_version}\n"));
        out.push_str(&format!(
            "Platform: {} ({})\n",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));

        let now_sec = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        out.push_str(&format!("Exported at unix timestamp: {now_sec}\n"));
        out.push_str(&format!("Recent app events: {}\n", entries_snapshot.len()));
        out.push_str("Content titles, service URLs, account tokens, and UUIDs are redacted.\n");

        if !extra_info.is_empty() {
            out.push_str("\n--- System & Hardware Info ---\n");
            for (key, val) in extra_info {
                out.push_str(&format!("{key}: {val}\n"));
            }
        }

        out.push_str("\n--- Event Log ---\n");
        if entries_snapshot.is_empty() {
            out.push_str("No diagnostic events have been recorded yet.\n");
        } else {
            for line in entries_snapshot {
                out.push_str(&line);
                out.push('\n');
            }
        }

        out
    }

    pub fn export_to_file<P: AsRef<Path>>(
        &self,
        path: P,
        app_version: &str,
        extra_info: &[(&str, &str)],
    ) -> std::io::Result<()> {
        let text = self.export_text(app_version, extra_info);
        if let Some(parent) = path.as_ref().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut file = File::create(path)?;
        file.write_all(text.as_bytes())?;
        Ok(())
    }
}

// Global logger instance
pub static GLOBAL_LOG: LazyLock<AppLog> = LazyLock::new(AppLog::new);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_sensitive_values() {
        let input = "Connecting to https://adult.ferges.eu:8096/emby with token=abc123xyz and password: mySecretPassword123";
        let redacted = redact(input);
        assert!(!redacted.contains("https://"));
        assert!(!redacted.contains("adult.ferges.eu"));
        assert!(!redacted.contains("abc123xyz"));
        assert!(!redacted.contains("mySecretPassword123"));
        assert!(redacted.contains("[URL]"));
        assert!(redacted.contains("token=[REDACTED]"));
        assert!(redacted.contains("password=[REDACTED]"));
    }

    #[test]
    fn redacts_uuids_and_titles() {
        let input = "Playing item_id=6ba7b810-9dad-11d1-80b4-00c04fd430c8 with title=Super Sensitive Video Title!";
        let redacted = redact(input);
        assert!(!redacted.contains("6ba7b810-9dad-11d1-80b4-00c04fd430c8"));
        assert!(!redacted.contains("Super Sensitive Video Title!"));
        assert!(redacted.contains("item_id=[REDACTED]"));
        assert!(redacted.contains("title=[REDACTED]"));

        let standalone_uuid = "Standalone session: 6ba7b810-9dad-11d1-80b4-00c04fd430c8 in log";
        let redacted2 = redact(standalone_uuid);
        assert!(redacted2.contains("[ID]"));
        assert!(!redacted2.contains("6ba7b810-9dad-11d1-80b4-00c04fd430c8"));
    }

    #[test]
    fn buffer_bounding_and_export() {
        let log = AppLog::new();
        for i in 0..600 {
            log.i("TEST", &format!("Event number {i}"));
        }

        let snapshot = log.entries.lock().unwrap();
        assert_eq!(snapshot.len(), 500);
        // The first 100 should have been dropped
        assert!(snapshot[0].contains("Event number 100"));
        drop(snapshot);

        let exported = log.export_text("0.1.0", &[("Decoder", "hwdec auto")]);
        assert!(exported.contains("Adult Hub diagnostics"));
        assert!(exported.contains("App version: 0.1.0"));
        assert!(exported.contains("Decoder: hwdec auto"));
        assert!(exported.contains("Event number 599"));
    }
}
