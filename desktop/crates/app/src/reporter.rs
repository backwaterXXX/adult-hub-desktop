use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const PROGRESS_COALESCE_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug)]
enum ReportCommand {
    AdultHubProgress {
        server_url: String,
        access_token: String,
        video_id: String,
        position_ms: u64,
        duration_ms: u64,
        is_played: bool,
    },
    Shutdown,
}

pub type PendingApiProgressRecord = (String, String, String, u64, u64, bool);

pub struct PlaybackReporter {
    server_auth: Arc<Mutex<Option<(String, String)>>>, // (base_url, access_token)
    command_tx: Sender<ReportCommand>,
    worker_handle: Option<JoinHandle<()>>,
    last_api_report_time: Arc<Mutex<Option<(String, Instant)>>>,
    pending_api_progress: Arc<Mutex<Option<PendingApiProgressRecord>>>,
    stop_signal: Arc<AtomicBool>,
}

impl PlaybackReporter {
    pub fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel::<ReportCommand>();
        let server_auth = Arc::new(Mutex::new(None));
        let last_api_report_time = Arc::new(Mutex::new(None));
        let pending_api_progress = Arc::new(Mutex::new(None));
        let stop_signal = Arc::new(AtomicBool::new(false));

        let worker_handle = {
            let stop_signal = stop_signal.clone();

            thread::Builder::new()
                .name("playback-reporter".into())
                .spawn(move || worker_loop(command_rx, stop_signal))
                .map_err(|e| {
                    log::error!("Failed to spawn playback-reporter thread: {e}");
                    e
                })
                .ok()
        };

        Self {
            server_auth,
            command_tx,
            worker_handle,
            last_api_report_time,
            pending_api_progress,
            stop_signal,
        }
    }

    pub fn set_auth(&self, base_url: Option<String>, token: Option<String>) {
        let mut lock = self
            .server_auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match (base_url, token) {
            (Some(url), Some(tok)) if !url.trim().is_empty() && !tok.trim().is_empty() => {
                *lock = Some((url, tok));
            }
            _ => *lock = None,
        }
    }

    /// Flush a coalesced progress update if the reporter is due to send one.
    pub fn tick_periodic(&self) {
        let should_flush = {
            let last = self
                .last_api_report_time
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            last.as_ref()
                .map(|(_, t)| t.elapsed() >= PROGRESS_COALESCE_DELAY)
                .unwrap_or(true)
        };
        if should_flush {
            self.flush_pending_progress();
        }
    }

    /// Asynchronously reports watch progress to the Adult Hub server via a
    /// background worker.
    ///
    /// Ignores unknown duration (0) or position < 5s, captures
    /// (server_url, access_token) in each queued command, and coalesces
    /// in-progress reports without blocking playback.
    pub fn report_api_progress(
        &self,
        video_id: &str,
        position_ms: u64,
        duration_ms: u64,
        is_played: bool,
    ) {
        let clean_id = video_id.trim();
        if clean_id.is_empty() {
            return;
        }
        if duration_ms == 0 || position_ms < 5000 {
            return;
        }
        let auth = {
            let lock = self
                .server_auth
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            lock.clone()
        };
        let Some((server_url, access_token)) = auth else {
            return;
        };

        if is_played {
            {
                let mut pending = self
                    .pending_api_progress
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *pending = None;
            }
            {
                let mut last = self
                    .last_api_report_time
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *last = Some((clean_id.to_string(), Instant::now()));
            }
            let _ = self.command_tx.send(ReportCommand::AdultHubProgress {
                server_url,
                access_token,
                video_id: clean_id.to_string(),
                position_ms,
                duration_ms,
                is_played: true,
            });
            return;
        }

        let mut last_lock = self
            .last_api_report_time
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut pending_lock = self
            .pending_api_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let should_send = match last_lock.as_ref() {
            Some((last_vid, last_time)) if last_vid == clean_id => {
                last_time.elapsed() >= PROGRESS_COALESCE_DELAY
            }
            _ => true,
        };

        if should_send {
            *pending_lock = None;
            *last_lock = Some((clean_id.to_string(), Instant::now()));
            let _ = self.command_tx.send(ReportCommand::AdultHubProgress {
                server_url,
                access_token,
                video_id: clean_id.to_string(),
                position_ms,
                duration_ms,
                is_played: false,
            });
        } else {
            *pending_lock = Some((
                server_url,
                access_token,
                clean_id.to_string(),
                position_ms,
                duration_ms,
                false,
            ));
        }
    }

    pub fn flush_pending_progress(&self) {
        let pending = {
            let mut lock = self
                .pending_api_progress
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            lock.take()
        };
        if let Some((server_url, access_token, video_id, position_ms, duration_ms, is_played)) =
            pending
        {
            {
                let mut last = self
                    .last_api_report_time
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *last = Some((video_id.clone(), Instant::now()));
            }
            let _ = self.command_tx.send(ReportCommand::AdultHubProgress {
                server_url,
                access_token,
                video_id,
                position_ms,
                duration_ms,
                is_played,
            });
        }
    }
}

impl Default for PlaybackReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PlaybackReporter {
    fn drop(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);
        self.flush_pending_progress();
        let _ = self.command_tx.send(ReportCommand::Shutdown);
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop(rx: Receiver<ReportCommand>, stop_signal: Arc<AtomicBool>) {
    loop {
        let cmd = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(cmd) => cmd,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stop_signal.load(Ordering::SeqCst) {
                    break;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        match cmd {
            ReportCommand::Shutdown => break,
            ReportCommand::AdultHubProgress {
                server_url,
                access_token,
                video_id,
                position_ms,
                duration_ms,
                is_played,
            } => match adult_hub_client::AdultHubClient::new(&server_url) {
                Ok(client) => {
                    let authed = client.with_token(access_token);
                    let req = adult_hub_client::WatchProgressUpsertRequest {
                        position_seconds: position_ms / 1000,
                        duration_seconds: duration_ms / 1000,
                        is_played,
                        played_percentage: None,
                        last_watched_at: None,
                    };
                    if let Err(err) = authed.put_watch_progress(&video_id, &req) {
                        log::warn!(
                            "Failed to report Adult Hub watch progress for {}: {}",
                            video_id,
                            err
                        );
                    }
                }
                Err(err) => {
                    log::warn!("Invalid Adult Hub service URL: {}", err);
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type ProgressServer = (
        String,
        Arc<Mutex<Vec<String>>>,
        Arc<Mutex<Vec<u64>>>,
        Arc<AtomicBool>,
        JoinHandle<()>,
    );

    fn spawn_progress_server() -> ProgressServer {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().unwrap();
        let server_url = format!("http://127.0.0.1:{}", addr.port());

        let requests = Arc::new(Mutex::new(Vec::new()));
        let positions = Arc::new(Mutex::new(Vec::new()));
        let stop_server = Arc::new(AtomicBool::new(false));
        let requests_clone = requests.clone();
        let positions_clone = positions.clone();
        let stop_clone = stop_server.clone();

        let server_thread = thread::spawn(move || {
            let _ = listener.set_nonblocking(true);
            while !stop_clone.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 2048];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n > 0 {
                            let req_str = String::from_utf8_lossy(&buf[..n]);
                            if let Some(first_line) = req_str.lines().next() {
                                requests_clone.lock().unwrap().push(first_line.to_string());
                            }
                            if let Some(body_start) = req_str.find("\r\n\r\n") {
                                let body = &req_str[body_start + 4..];
                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(body)
                                    && let Some(pos) =
                                        val.get("position_seconds").and_then(|p| p.as_u64())
                                {
                                    positions_clone.lock().unwrap().push(pos);
                                }
                            }
                            let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}";
                            let _ = stream.write_all(response.as_bytes());
                            let _ = stream.flush();
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        (server_url, requests, positions, stop_server, server_thread)
    }

    #[test]
    fn progress_reporting_sends_put_to_watch_endpoint() {
        let (server_url, requests, _positions, stop_server, server_thread) =
            spawn_progress_server();
        let reporter = PlaybackReporter::new();
        reporter.set_auth(Some(server_url), Some("mock_token".into()));

        reporter.report_api_progress("video-test-async", 20000, 60000, false);

        drop(reporter);
        stop_server.store(true, Ordering::SeqCst);
        let _ = server_thread.join();

        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("PUT /api/v1/user/watch/video-test-async"));
    }

    #[test]
    fn progress_reporting_ignores_invalid_and_zero_position() {
        let (server_url, requests, _positions, stop_server, server_thread) =
            spawn_progress_server();
        let reporter = PlaybackReporter::new();
        reporter.set_auth(Some(server_url), Some("mock_token".into()));

        // 1. Zero position ignored
        reporter.report_api_progress("video-zero", 0, 60000, false);
        // 2. Position < 5s (4999 ms) ignored
        reporter.report_api_progress("video-sub5s", 4999, 60000, false);
        // 3. Unknown duration (0 ms) ignored
        reporter.report_api_progress("video-zerodur", 10000, 0, false);

        drop(reporter);
        stop_server.store(true, Ordering::SeqCst);
        let _ = server_thread.join();

        assert!(requests.lock().unwrap().is_empty());
    }

    #[test]
    fn progress_reporting_captures_token_across_account_switch() {
        let (server_url, requests, _positions, stop_server, server_thread) =
            spawn_progress_server();
        let reporter = PlaybackReporter::new();

        // Account 1 login
        reporter.set_auth(Some(server_url.clone()), Some("token_account_alpha".into()));
        reporter.report_api_progress("video-acc-1", 10000, 60000, false);

        // Account 2 switch
        reporter.set_auth(Some(server_url), Some("token_account_beta".into()));
        reporter.report_api_progress("video-acc-2", 12000, 60000, false);

        drop(reporter);
        stop_server.store(true, Ordering::SeqCst);
        let _ = server_thread.join();

        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("PUT /api/v1/user/watch/video-acc-1"));
        assert!(requests[1].starts_with("PUT /api/v1/user/watch/video-acc-2"));
    }

    #[test]
    fn progress_reporting_coalesces_updates() {
        let (server_url, _requests, positions, stop_server, server_thread) =
            spawn_progress_server();
        let reporter = PlaybackReporter::new();
        reporter.set_auth(Some(server_url), Some("mock_token".into()));

        // Rapid UI poll calls (5 calls in rapid succession)
        reporter.report_api_progress("video-coalesce", 5000, 60000, false);
        reporter.report_api_progress("video-coalesce", 5200, 60000, false);
        reporter.report_api_progress("video-coalesce", 5400, 60000, false);
        reporter.report_api_progress("video-coalesce", 5600, 60000, false);
        reporter.report_api_progress("video-coalesce", 5800, 60000, false);

        // Flush on drop
        drop(reporter);
        stop_server.store(true, Ordering::SeqCst);
        let _ = server_thread.join();

        let positions = positions.lock().unwrap().clone();
        assert!(positions.len() <= 2);
        assert_eq!(*positions.first().unwrap(), 5);
        if positions.len() == 2 {
            assert_eq!(*positions.last().unwrap(), 5);
        }
    }
}
