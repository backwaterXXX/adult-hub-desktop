use adult_hub_client::{
    JellyfinClient, PlayMethod, PlaybackProgressInfo, PlaybackStartInfo, PlaybackStopInfo,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[allow(dead_code)]
const REPORTING_INTERVAL: Duration = Duration::from_secs(10);
const PROGRESS_COALESCE_DELAY: Duration = Duration::from_millis(500);
#[allow(dead_code)]
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

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
    Shutdown,
}

pub type PendingProgressRecord = (SessionInfo, u64, bool, Instant);

pub struct PlaybackReporter {
    _client: Arc<JellyfinClient>,
    server_auth: Arc<Mutex<Option<(String, String)>>>, // (base_url, access_token)
    session_counter: AtomicU64,
    current_session: Arc<Mutex<Option<SessionInfo>>>,
    command_tx: Sender<ReportCommand>,
    worker_handle: Option<JoinHandle<()>>,
    pending_progress: Arc<Mutex<Option<PendingProgressRecord>>>,
    stop_signal: Arc<AtomicBool>,
}

impl PlaybackReporter {
    pub fn new(client: Arc<JellyfinClient>) -> Self {
        let (command_tx, command_rx) = mpsc::channel::<ReportCommand>();
        let server_auth = Arc::new(Mutex::new(None));
        let current_session = Arc::new(Mutex::new(None));
        let pending_progress = Arc::new(Mutex::new(None));
        let stop_signal = Arc::new(AtomicBool::new(false));

        let worker_handle = {
            let client = client.clone();
            let server_auth = server_auth.clone();
            let current_session = current_session.clone();
            let stop_signal = stop_signal.clone();

            thread::Builder::new()
                .name("playback-reporter".into())
                .spawn(move || {
                    worker_loop(client, server_auth, current_session, command_rx, stop_signal)
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
            stop_signal,
        }
    }

    pub fn set_auth(&self, base_url: Option<String>, token: Option<String>) {
        let mut lock = self.server_auth.lock().unwrap();
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

        let mut lock = self.current_session.lock().unwrap();
        *lock = Some(session);
    }

    pub fn report_start(&self, position_ms: u64, can_seek: bool) {
        let mut lock = self.current_session.lock().unwrap();
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
        let session = {
            let lock = self.current_session.lock().unwrap();
            lock.clone()
        };
        let Some(session) = session else { return };
        if !session.has_started {
            return;
        }

        // Flush directly or via debounce
        let mut pending = self.pending_progress.lock().unwrap();
        *pending = Some((session, position_ms, is_paused, Instant::now()));
    }

    pub fn tick_periodic(&self, current_pos_ms: u64, is_paused: bool) {
        // 1. Check pending coalesced progress
        let should_send_pending = {
            let mut pending = self.pending_progress.lock().unwrap();
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
                let lock = self.current_session.lock().unwrap();
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
        // Flush any pending progress first
        let pending = {
            let mut lock = self.pending_progress.lock().unwrap();
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
            let mut lock = self.current_session.lock().unwrap();
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

    #[allow(dead_code)]
    pub fn current_session_id(&self) -> Option<u64> {
        let lock = self.current_session.lock().unwrap();
        lock.as_ref().map(|s| s.session_id)
    }

    #[allow(dead_code)]
    pub fn is_active_session(&self, session_id: u64) -> bool {
        let lock = self.current_session.lock().unwrap();
        lock.as_ref().is_some_and(|s| s.session_id == session_id)
    }
}

impl Drop for PlaybackReporter {
    fn drop(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);
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
) {
    while !stop_signal.load(Ordering::SeqCst) {
        let cmd = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(cmd) => cmd,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        match cmd {
            ReportCommand::Shutdown => break,
            ReportCommand::Start { session, position_ms } => {
                let is_current = {
                    let lock = current_session.lock().unwrap();
                    lock.as_ref().is_some_and(|s| s.session_id == session.session_id)
                };
                if !is_current {
                    continue;
                }

                if uuid::Uuid::parse_str(&session.item_id).is_err() {
                    continue;
                }

                let auth = {
                    let lock = server_auth.lock().unwrap();
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
                    let lock = current_session.lock().unwrap();
                    lock.as_ref().is_some_and(|s| s.session_id == session.session_id)
                };
                if !is_current {
                    continue;
                }

                if uuid::Uuid::parse_str(&session.item_id).is_err() {
                    continue;
                }

                let auth = {
                    let lock = server_auth.lock().unwrap();
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
            ReportCommand::Stop { session, position_ms } => {
                // Stop is always sent for the session even if current session has changed
                if uuid::Uuid::parse_str(&session.item_id).is_err() {
                    continue;
                }

                let auth = {
                    let lock = server_auth.lock().unwrap();
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lifecycle_and_id_increment() {
        let client = Arc::new(JellyfinClient::new("dev-test"));
        let reporter = PlaybackReporter::new(client);

        assert_eq!(reporter.current_session_id(), None);

        reporter.start_session("item-1", None, None, PlayMethod::DirectPlay, 0);
        assert_eq!(reporter.current_session_id(), Some(1));
        assert!(reporter.is_active_session(1));
        assert!(!reporter.is_active_session(2));

        reporter.start_session("item-2", None, None, PlayMethod::DirectPlay, 5000);
        assert_eq!(reporter.current_session_id(), Some(2));
        assert!(!reporter.is_active_session(1));
        assert!(reporter.is_active_session(2));

        reporter.stop_session(10000);
        assert_eq!(reporter.current_session_id(), None);
    }

    #[test]
    fn reports_start_only_once() {
        let client = Arc::new(JellyfinClient::new("dev-test"));
        let reporter = PlaybackReporter::new(client);

        reporter.start_session("item-1", None, None, PlayMethod::DirectPlay, 0);
        reporter.report_start(1000, true);

        {
            let lock = reporter.current_session.lock().unwrap();
            let session = lock.as_ref().unwrap();
            assert!(session.has_started);
            assert!(session.can_seek);
        }

        // Second report_start should be ignored
        reporter.report_start(2000, false);
        {
            let lock = reporter.current_session.lock().unwrap();
            let session = lock.as_ref().unwrap();
            assert!(session.can_seek); // remains true
        }
    }
}
