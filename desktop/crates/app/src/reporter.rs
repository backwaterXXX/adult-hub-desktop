use adult_hub_client::{
    JellyfinClient, PlayMethod, PlaybackProgressInfo, PlaybackStartInfo, PlaybackStopInfo,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const PROGRESS_COALESCE_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: u64,
    pub item_id: String,
    pub media_source_id: Option<String>,
    pub play_session_id: Option<String>,
    pub play_method: PlayMethod,
    pub can_seek: bool,
    pub has_started: bool,
}

#[derive(Debug)]
enum ReportCommand {
    Start {
        session: SessionInfo,
        position_ms: u64,
    },
    Progress {
        session: SessionInfo,
        position_ms: u64,
        is_paused: bool,
    },
    Stop {
        session: SessionInfo,
        position_ms: u64,
    },
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

pub type PendingProgressRecord = (SessionInfo, u64, bool, Instant);
pub type PendingApiProgressRecord = (String, String, String, u64, u64, bool);

pub struct PlaybackReporter {
    _client: Arc<JellyfinClient>,
    server_auth: Arc<Mutex<Option<(String, String)>>>, // (base_url, access_token)
    session_counter: AtomicU64,
    current_session: Arc<Mutex<Option<SessionInfo>>>,
    command_tx: Sender<ReportCommand>,
    worker_handle: Option<JoinHandle<()>>,
    pending_progress: Arc<Mutex<Option<PendingProgressRecord>>>,
    last_api_report_time: Arc<Mutex<Option<(String, Instant)>>>,
    pending_api_progress: Arc<Mutex<Option<PendingApiProgressRecord>>>,
    stop_signal: Arc<AtomicBool>,
    api_mode: Arc<AtomicBool>,
}

impl PlaybackReporter {
    pub fn new(client: Arc<JellyfinClient>) -> Self {
        let (command_tx, command_rx) = mpsc::channel::<ReportCommand>();
        let server_auth = Arc::new(Mutex::new(None));
        let current_session = Arc::new(Mutex::new(None));
        let pending_progress = Arc::new(Mutex::new(None));
        let last_api_report_time = Arc::new(Mutex::new(None));
        let pending_api_progress = Arc::new(Mutex::new(None));
        let stop_signal = Arc::new(AtomicBool::new(false));
        let api_mode = Arc::new(AtomicBool::new(false));

        let worker_handle = {
            let client = client.clone();
            let server_auth = server_auth.clone();
            let current_session = current_session.clone();
            let stop_signal = stop_signal.clone();
            let api_mode = api_mode.clone();

            thread::Builder::new()
                .name("playback-reporter".into())
                .spawn(move || {
                    worker_loop(
                        client,
                        server_auth,
                        current_session,
                        command_rx,
                        stop_signal,
                        api_mode,
                    )
                })
                .map_err(|e| {
                    log::error!("Failed to spawn playback-reporter thread: {e}");
                    e
                })
                .ok()
        };

        Self {
            _client: client,
            server_auth,
            session_counter: AtomicU64::new(0),
            current_session,
            command_tx,
            worker_handle,
            pending_progress,
            last_api_report_time,
            pending_api_progress,
            stop_signal,
            api_mode,
        }
    }

    pub fn set_api_mode(&self, enabled: bool) {
        self.api_mode.store(enabled, Ordering::SeqCst);
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

    pub fn start_session(
        &self,
        item_id: &str,
        media_source_id: Option<String>,
        play_session_id: Option<String>,
        play_method: PlayMethod,
        current_pos_ms: u64,
    ) {
        if self.api_mode.load(Ordering::SeqCst) {
            return;
        }
        // Stop any active session first
        self.stop_session(current_pos_ms);

        let new_id = self.session_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let session = SessionInfo {
            session_id: new_id,
            item_id: item_id.to_string(),
            media_source_id,
            play_session_id,
            play_method,
            can_seek: false,
            has_started: false,
        };

        let mut lock = self
            .current_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *lock = Some(session);
    }

    pub fn report_start(&self, position_ms: u64, can_seek: bool) {
        if self.api_mode.load(Ordering::SeqCst) {
            return;
        }
        let mut lock = self
            .current_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(session) = lock.as_mut() else { return };
        if session.has_started {
            return;
        }
        session.can_seek = can_seek;
        session.has_started = true;
        let session_clone = session.clone();

        let _ = self.command_tx.send(ReportCommand::Start {
            session: session_clone,
            position_ms,
        });
    }

    pub fn report_progress_now(&self, position_ms: u64, is_paused: bool) {
        if self.api_mode.load(Ordering::SeqCst) {
            return;
        }
        let session = {
            let lock = self
                .current_session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            lock.clone()
        };
        let Some(session) = session else { return };
        if !session.has_started {
            return;
        }

        // Flush directly or via debounce
        let mut pending = self
            .pending_progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *pending = Some((session, position_ms, is_paused, Instant::now()));
    }

    pub fn tick_periodic(&self, current_pos_ms: u64, is_paused: bool) {
        if self.api_mode.load(Ordering::SeqCst) {
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
                self.flush_pending_api_progress();
            }
            return;
        }
        // 1. Check pending coalesced progress
        let should_send_pending = {
            let mut pending = self
                .pending_progress
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((session, pos, paused, timestamp)) = pending.as_ref() {
                if timestamp.elapsed() >= PROGRESS_COALESCE_DELAY {
                    let cmd = ReportCommand::Progress {
                        session: session.clone(),
                        position_ms: *pos,
                        is_paused: *paused,
                    };
                    let _ = self.command_tx.send(cmd);
                    *pending = None;
                    false
                } else {
                    false
                }
            } else {
                true
            }
        };

        // 2. If no pending coalesced update, send periodic report if session is started
        if should_send_pending {
            let session = {
                let lock = self
                    .current_session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                lock.clone()
            };
            if let Some(session) = session {
                if session.has_started {
                    let _ = self.command_tx.send(ReportCommand::Progress {
                        session,
                        position_ms: current_pos_ms,
                        is_paused,
                    });
                }
            }
        }
    }

    pub fn stop_session(&self, final_position_ms: u64) {
        if self.api_mode.load(Ordering::SeqCst) {
            self.flush_pending_api_progress();
            return;
        }

        // Flush any pending progress first
        let pending = {
            let mut lock = self
                .pending_progress
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            lock.take()
        };
        if let Some((session, pos, paused, _)) = pending {
            let _ = self.command_tx.send(ReportCommand::Progress {
                session,
                position_ms: pos,
                is_paused: paused,
            });
        }

        let session = {
            let mut lock = self
                .current_session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            lock.take()
        };

        if let Some(session) = session {
            if session.has_started {
                let _ = self.command_tx.send(ReportCommand::Stop {
                    session,
                    position_ms: final_position_ms,
                });
            }
        }
    }

    pub fn is_active_for_item(&self, item_id: &str) -> bool {
        let lock = self
            .current_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lock.as_ref().is_some_and(|s| s.item_id == item_id)
    }

    /// Asynchronously reports watch progress to Adult Hub server via background worker when in API mode.
    ///
    /// Ignores unknown duration (0) or position < 5s, captures (server_url, access_token) in each queued command,
    /// and coalesces in-progress reports to existing reporter cadence without blocking playback.
    pub fn report_api_progress(
        &self,
        video_id: &str,
        position_ms: u64,
        duration_ms: u64,
        is_played: bool,
    ) {
        if !self.api_mode.load(Ordering::SeqCst) {
            return;
        }
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

    fn flush_pending_api_progress(&self) {
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

impl Drop for PlaybackReporter {
    fn drop(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);
        self.flush_pending_api_progress();
        let _ = self.command_tx.send(ReportCommand::Shutdown);
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop(
    client: Arc<JellyfinClient>,
    server_auth: Arc<Mutex<Option<(String, String)>>>,
    current_session: Arc<Mutex<Option<SessionInfo>>>,
    rx: Receiver<ReportCommand>,
    stop_signal: Arc<AtomicBool>,
    api_mode: Arc<AtomicBool>,
) {
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

        if api_mode.load(Ordering::SeqCst)
            && !matches!(
                cmd,
                ReportCommand::Shutdown
                    | ReportCommand::Stop { .. }
                    | ReportCommand::AdultHubProgress { .. }
            )
        {
            continue;
        }

        match cmd {
            ReportCommand::Shutdown => break,
            ReportCommand::Start {
                session,
                position_ms,
            } => {
                let is_current = {
                    let lock = current_session
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    lock.as_ref()
                        .is_some_and(|s| s.session_id == session.session_id)
                };
                if !is_current {
                    continue;
                }

                if uuid::Uuid::parse_str(&session.item_id).is_err() {
                    continue;
                }

                let auth = {
                    let lock = server_auth
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    lock.clone()
                };
                if let Some((base_url, token)) = auth {
                    let info = PlaybackStartInfo {
                        item_id: session.item_id,
                        media_source_id: session.media_source_id,
                        position_ticks: (position_ms as i64) * 10_000,
                        play_session_id: session.play_session_id,
                        can_seek: session.can_seek,
                        is_muted: false,
                        is_paused: false,
                        repeat_mode: "RepeatNone".into(),
                        playback_order: "Default".into(),
                        play_method: session.play_method,
                    };
                    let _ = client.report_playback_start(&base_url, &token, &info);
                }
            }
            ReportCommand::Progress {
                session,
                position_ms,
                is_paused,
            } => {
                let is_current = {
                    let lock = current_session
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    lock.as_ref()
                        .is_some_and(|s| s.session_id == session.session_id)
                };
                if !is_current {
                    continue;
                }

                if uuid::Uuid::parse_str(&session.item_id).is_err() {
                    continue;
                }

                let auth = {
                    let lock = server_auth
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    lock.clone()
                };
                if let Some((base_url, token)) = auth {
                    let info = PlaybackProgressInfo {
                        item_id: session.item_id,
                        media_source_id: session.media_source_id,
                        position_ticks: (position_ms as i64) * 10_000,
                        play_session_id: session.play_session_id,
                        can_seek: session.can_seek,
                        is_muted: false,
                        is_paused,
                        repeat_mode: "RepeatNone".into(),
                        playback_order: "Default".into(),
                        play_method: session.play_method,
                    };
                    let _ = client.report_playback_progress(&base_url, &token, &info);
                }
            }
            ReportCommand::Stop {
                session,
                position_ms,
            } => {
                // Stop is always sent for the session even if current session has changed
                if uuid::Uuid::parse_str(&session.item_id).is_err() {
                    continue;
                }

                let auth = {
                    let lock = server_auth
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    lock.clone()
                };
                if let Some((base_url, token)) = auth {
                    let info = PlaybackStopInfo {
                        item_id: session.item_id,
                        media_source_id: session.media_source_id,
                        position_ticks: (position_ms as i64) * 10_000,
                        play_session_id: session.play_session_id,
                        failed: false,
                    };
                    // Bounded final stop attempt
                    let _ = client.report_playback_stopped(&base_url, &token, &info);
                }
            }
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

    #[test]
    fn reports_start_only_once() {
        let client = Arc::new(JellyfinClient::new("dev-test"));
        let reporter = PlaybackReporter::new(client);

        reporter.start_session("item-1", None, None, PlayMethod::DirectPlay, 0);
        reporter.report_start(1000, true);

        {
            let lock = reporter
                .current_session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let session = lock.as_ref().unwrap();
            assert!(session.has_started);
            assert!(session.can_seek);
        }

        // Second report_start should be ignored
        reporter.report_start(2000, false);
        {
            let lock = reporter
                .current_session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let session = lock.as_ref().unwrap();
            assert!(session.can_seek); // remains true
        }
    }

    #[test]
    fn api_mode_suppresses_reporting() {
        let client = Arc::new(JellyfinClient::new("dev-test"));
        let reporter = PlaybackReporter::new(client);
        reporter.set_api_mode(true);

        // When API mode is active, starting a session is a no-op
        reporter.start_session("item-1", None, None, PlayMethod::DirectPlay, 0);
        assert!(!reporter.is_active_for_item("item-1"));
    }

    #[test]
    fn test_reporting_stop_queue_behavior_across_mode_transition() {
        let client = Arc::new(JellyfinClient::new("dev-test"));
        let reporter = PlaybackReporter::new(client);

        let jf_item_id = "00000000-0000-0000-0000-000000000001";
        reporter.start_session(jf_item_id, None, None, PlayMethod::DirectPlay, 0);
        assert!(reporter.is_active_for_item(jf_item_id));
        reporter.report_start(1000, true);

        // 1. Transition: Stop Jellyfin session while Jellyfin mode is still active
        reporter.stop_session(5000);
        assert!(!reporter.is_active_for_item(jf_item_id));

        // Enable Adult Hub API mode after stop is queued
        reporter.set_api_mode(true);

        // 2. In API mode, Adult Hub playback never starts a Jellyfin session
        let ah_item_id = "adult-hub-item-42";
        reporter.start_session(ah_item_id, None, None, PlayMethod::DirectPlay, 0);
        assert!(!reporter.is_active_for_item(ah_item_id));

        // Additional events in API mode are suppressed
        reporter.report_start(2000, true);
        reporter.report_progress_now(3000, false);
        reporter.tick_periodic(4000, false);

        // Stopping an Adult Hub item does not send Jellyfin stop
        reporter.stop_session(5000);
        assert!(!reporter.is_active_for_item(ah_item_id));

        // 3. Switching back from Adult Hub to Jellyfin mode
        reporter.set_api_mode(false);
        assert!(!reporter.is_active_for_item(ah_item_id));

        // When switching back, no session was active, so stop does not send events for Adult Hub
        reporter.stop_session(0);
        assert!(!reporter.is_active_for_item(ah_item_id));
    }

    #[test]
    fn test_worker_queue_delivers_jellyfin_stop_after_mode_change_and_drops_start_progress() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().unwrap();
        let server_url = format!("http://127.0.0.1:{}", addr.port());

        let delivered_paths = Arc::new(Mutex::new(Vec::new()));
        let delivered_clone = delivered_paths.clone();
        let stop_server = Arc::new(AtomicBool::new(false));
        let stop_clone = stop_server.clone();

        let server_thread = thread::spawn(move || {
            let _ = listener.set_nonblocking(true);
            while !stop_clone.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 1024];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n > 0 {
                            let req_str = String::from_utf8_lossy(&buf[..n]);
                            if let Some(first_line) = req_str.lines().next() {
                                let parts: Vec<&str> = first_line.split_whitespace().collect();
                                if parts.len() >= 2 {
                                    delivered_clone.lock().unwrap().push(parts[1].to_string());
                                }
                            }
                            let response = "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
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

        let client = Arc::new(JellyfinClient::new("dev-test"));
        let reporter = PlaybackReporter::new(client);
        reporter.set_auth(Some(server_url), Some("mock_token".into()));

        let jf_item_id = "00000000-0000-0000-0000-000000000001";
        let session = SessionInfo {
            session_id: 1,
            item_id: jf_item_id.to_string(),
            media_source_id: None,
            play_session_id: None,
            play_method: PlayMethod::DirectPlay,
            can_seek: true,
            has_started: true,
        };

        // Transition mode to API mode
        reporter.set_api_mode(true);

        // Send Start and Progress commands directly into the queue: worker_loop must drop them
        let _ = reporter.command_tx.send(ReportCommand::Start {
            session: session.clone(),
            position_ms: 1000,
        });
        let _ = reporter.command_tx.send(ReportCommand::Progress {
            session: session.clone(),
            position_ms: 2000,
            is_paused: false,
        });

        // Send Jellyfin Stop command: worker_loop must still deliver Stop even after mode change
        let _ = reporter.command_tx.send(ReportCommand::Stop {
            session,
            position_ms: 3000,
        });

        // Dropping reporter sends Shutdown and waits for worker thread to finish processing all queued items
        drop(reporter);

        stop_server.store(true, Ordering::SeqCst);
        let _ = server_thread.join();

        let paths = delivered_paths.lock().unwrap().clone();
        assert_eq!(paths, vec!["/Sessions/Playing/Stopped"]);
    }

    #[test]
    fn test_api_mode_progress_reporting() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().unwrap();
        let server_url = format!("http://127.0.0.1:{}", addr.port());

        let delivered = Arc::new(Mutex::new(Vec::new()));
        let delivered_clone = delivered.clone();
        let stop_server = Arc::new(AtomicBool::new(false));
        let stop_clone = stop_server.clone();

        let server_thread = thread::spawn(move || {
            let _ = listener.set_nonblocking(true);
            while !stop_clone.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 1024];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n > 0 {
                            let req_str = String::from_utf8_lossy(&buf[..n]);
                            if let Some(first_line) = req_str.lines().next() {
                                delivered_clone.lock().unwrap().push(first_line.to_string());
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

        let client = Arc::new(JellyfinClient::new("dev-test"));
        let reporter = PlaybackReporter::new(client);
        reporter.set_auth(Some(server_url), Some("mock_token".into()));

        // In Jellyfin mode, report_api_progress is a no-op
        reporter.report_api_progress("video-test-1", 5000, 60000, false);
        thread::sleep(Duration::from_millis(50));
        assert!(delivered.lock().unwrap().is_empty());

        // Enable API mode
        reporter.set_api_mode(true);

        // Asynchronous test
        reporter.report_api_progress("video-test-async", 20000, 60000, false);

        // Wait for worker thread to deliver commands
        drop(reporter);

        stop_server.store(true, Ordering::SeqCst);
        let _ = server_thread.join();

        let requests = delivered.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("PUT /api/v1/user/watch/video-test-async"));
    }

    #[test]
    fn test_api_mode_progress_ignores_invalid_and_zero_position() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().unwrap();
        let server_url = format!("http://127.0.0.1:{}", addr.port());

        let delivered = Arc::new(Mutex::new(Vec::new()));
        let delivered_clone = delivered.clone();
        let stop_server = Arc::new(AtomicBool::new(false));
        let stop_clone = stop_server.clone();

        let server_thread = thread::spawn(move || {
            let _ = listener.set_nonblocking(true);
            while !stop_clone.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 1024];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n > 0 {
                            let req_str = String::from_utf8_lossy(&buf[..n]);
                            if let Some(first_line) = req_str.lines().next() {
                                delivered_clone.lock().unwrap().push(first_line.to_string());
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

        let client = Arc::new(JellyfinClient::new("dev-test"));
        let reporter = PlaybackReporter::new(client);
        reporter.set_auth(Some(server_url), Some("mock_token".into()));
        reporter.set_api_mode(true);

        // 1. Zero position ignored
        reporter.report_api_progress("video-zero", 0, 60000, false);
        // 2. Position < 5s (4999 ms) ignored
        reporter.report_api_progress("video-sub5s", 4999, 60000, false);
        // 3. Unknown duration (0 ms) ignored
        reporter.report_api_progress("video-zerodur", 10000, 0, false);

        drop(reporter);

        stop_server.store(true, Ordering::SeqCst);
        let _ = server_thread.join();

        assert!(delivered.lock().unwrap().is_empty());
    }

    #[test]
    fn test_api_mode_progress_captures_token_across_account_switch() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().unwrap();
        let server_url = format!("http://127.0.0.1:{}", addr.port());

        let delivered_auths = Arc::new(Mutex::new(Vec::new()));
        let delivered_clone = delivered_auths.clone();
        let stop_server = Arc::new(AtomicBool::new(false));
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
                            for line in req_str.lines() {
                                if line.to_ascii_lowercase().starts_with("authorization:") {
                                    delivered_clone
                                        .lock()
                                        .unwrap()
                                        .push(line.trim().to_string());
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

        let client = Arc::new(JellyfinClient::new("dev-test"));
        let reporter = PlaybackReporter::new(client);
        reporter.set_api_mode(true);

        // Account 1 login
        reporter.set_auth(Some(server_url.clone()), Some("token_account_alpha".into()));
        reporter.report_api_progress("video-acc-1", 10000, 60000, false);

        // Account 2 switch
        reporter.set_auth(Some(server_url), Some("token_account_beta".into()));
        reporter.report_api_progress("video-acc-2", 12000, 60000, false);

        drop(reporter);

        stop_server.store(true, Ordering::SeqCst);
        let _ = server_thread.join();

        let auths = delivered_auths.lock().unwrap().clone();
        assert_eq!(auths.len(), 2);
        assert_eq!(auths[0], "authorization: Bearer token_account_alpha");
        assert_eq!(auths[1], "authorization: Bearer token_account_beta");
    }

    #[test]
    fn test_api_mode_progress_coalesces_updates() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().unwrap();
        let server_url = format!("http://127.0.0.1:{}", addr.port());

        let delivered_positions = Arc::new(Mutex::new(Vec::new()));
        let delivered_clone = delivered_positions.clone();
        let stop_server = Arc::new(AtomicBool::new(false));
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
                            if let Some(body_start) = req_str.find("\r\n\r\n") {
                                let body = &req_str[body_start + 4..];
                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(body) {
                                    if let Some(pos) =
                                        val.get("position_seconds").and_then(|p| p.as_u64())
                                    {
                                        delivered_clone.lock().unwrap().push(pos);
                                    }
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

        let client = Arc::new(JellyfinClient::new("dev-test"));
        let reporter = PlaybackReporter::new(client);
        reporter.set_auth(Some(server_url), Some("mock_token".into()));
        reporter.set_api_mode(true);

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

        let positions = delivered_positions.lock().unwrap().clone();
        // Rather than 5 requests, rapid calls are coalesced to the initial send (5s) and final pending flush (5s or 5.8s -> 5)
        assert!(positions.len() <= 2);
        assert_eq!(*positions.first().unwrap(), 5);
        if positions.len() == 2 {
            assert_eq!(*positions.last().unwrap(), 5);
        }
    }
}
