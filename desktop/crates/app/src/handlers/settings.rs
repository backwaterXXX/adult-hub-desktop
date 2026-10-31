use crate::catalog::set_catalog;
use crate::context::AppContext;
use crate::playback::{export_file_dialog, pick_config_file, show_error};
use crate::{LibraryOptionData, MainWindow};
use adult_hub_client::{GetItemsQuery, JellyfinClient, ServiceUrlPolicy, ServiceUrlValidation};
use slint::ComponentHandle;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

fn refresh_content_after_filter_change(ui: &MainWindow) {
    match ui.get_last_content_screen().as_str() {
        "home" => ui.invoke_refresh_home(),
        "browse" if !ui.get_is_catalog_search_results() => {
            ui.invoke_filter_category(ui.get_active_category());
        }
        "browse" => ui.invoke_search_catalog(ui.get_search_query()),
        "channels" => {
            let cat = ui.get_active_channel_category();
            if !cat.is_empty() {
                ui.invoke_select_channel_category(cat);
            } else {
                ui.invoke_refresh_channel_categories();
            }
        }
        _ => {}
    }
}

/// Connection diagnostics shown in Settings → Integrations. Every probe
/// reports one of these states plus a short explanation, so the UI can never
/// imply success for a check that was skipped, timed out or never ran.
const CHECK_NOT_CHECKED: &str = "not-checked";
const CHECK_CHECKING: &str = "checking";
const CHECK_CONNECTED: &str = "connected";
const CHECK_PARTIAL: &str = "partial";
const CHECK_ERROR: &str = "error";
const CHECK_SKIPPED: &str = "not-configured";

struct Check {
    state: &'static str,
    detail: String,
}

impl Check {
    fn connected(detail: impl Into<String>) -> Self {
        Self {
            state: CHECK_CONNECTED,
            detail: detail.into(),
        }
    }

    fn error(detail: impl Into<String>) -> Self {
        Self {
            state: CHECK_ERROR,
            detail: redact_diagnostics(&detail.into()),
        }
    }

    fn skipped(detail: impl Into<String>) -> Self {
        Self {
            state: CHECK_SKIPPED,
            detail: detail.into(),
        }
    }
}

/// Diagnostic text must never carry credentials: image and stream URLs embed
/// `api_key=`, and transport errors echo the full request URL and auth headers.
fn redact_diagnostics(text: &str) -> String {
    const SECRETS: [&str; 6] = [
        "api_key",
        "apikey",
        "x-emby-token",
        "x-emby-authorization",
        "authorization",
        "token",
    ];
    let lowered = text.to_ascii_lowercase();
    let lowered_bytes = lowered.as_bytes();
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let key = SECRETS
            .iter()
            .find(|key| lowered_bytes[index..].starts_with(key.as_bytes()));
        let Some(key) = key else {
            out.push(bytes[index]);
            index += 1;
            continue;
        };
        out.extend_from_slice(&bytes[index..index + key.len()]);
        index += key.len();
        while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
            out.push(bytes[index]);
            index += 1;
        }
        let value_form = index < bytes.len() && (bytes[index] == b'=' || bytes[index] == b':');
        if !value_form {
            continue;
        }
        out.push(bytes[index]);
        index += 1;
        while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
            out.push(bytes[index]);
            index += 1;
        }
        if index >= bytes.len() {
            continue;
        }
        if matches!(bytes[index], b'"' | b'\'') {
            let quote = bytes[index];
            out.push(quote);
            index += 1;
            out.extend_from_slice(b"[redacted]");
            while index < bytes.len() && bytes[index] != quote {
                index += 1;
            }
            continue;
        }
        out.extend_from_slice(b"[redacted]");
        while index < bytes.len()
            && !matches!(
                bytes[index],
                b' ' | b'\t' | b'&' | b'"' | b'\'' | b',' | b';' | b')' | b'\n' | b'\r'
            )
        {
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Run a probe on its own thread and give up after `secs`, so a stalled socket
/// can never leave Settings hanging on "Checking…".
fn timed<T: Send + 'static>(
    secs: u64,
    probe: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(probe());
    });
    rx.recv_timeout(std::time::Duration::from_secs(secs))
        .map_err(|_| format!("no response within {secs}s"))
}

fn jellyfin_thumbnail_probe(
    jellyfin: &JellyfinClient,
    session: &adult_hub_storage::Session,
) -> Check {
    let page = match jellyfin.get_items(
        &session.server_url,
        &session.user_id,
        &session.access_token,
        &GetItemsQuery {
            start_index: 0,
            limit: 8,
            sort_by: "DateCreated",
            sort_descending: true,
            include_item_types: Some("Movie"),
            ..Default::default()
        },
    ) {
        Ok(page) => page,
        Err(_) => return Check::error("Jellyfin image sample: failed to fetch movies"),
    };
    let cards = page
        .items
        .iter()
        .take(5)
        .map(|item| crate::catalog::tv_item_to_card_for_server(item, &session.server_url))
        .collect::<Vec<_>>();
    if cards.is_empty() {
        return Check::skipped("Jellyfin image sample: no movies in response");
    }

    let mut loaded = 0;
    let mut without_url = 0;
    let mut failures: Vec<String> = Vec::new();
    for card in &cards {
        let candidates = [
            card.thumbnail_url.to_string(),
            card.fallback_thumbnail_url.to_string(),
            card.alternate_thumbnail_url.to_string(),
        ];
        let candidates = candidates
            .into_iter()
            .filter(|url| !url.is_empty())
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            without_url += 1;
            continue;
        }
        match fetch_any_fresh_jellyfin_image(&candidates, &session.access_token) {
            Ok(()) => loaded += 1,
            Err(failure) => failures.push(format!(
                "{label} ({stage})",
                label = failure.label(),
                stage = failure.stage()
            )),
        }
    }

    let failure_note = if failures.is_empty() {
        String::new()
    } else {
        let mut unique = failures
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        // Keep the one-line check detail readable: the full per-item trace is
        // in the log, the check shows the distinct categories.
        let shown = unique
            .drain(..2.min(unique.len()))
            .collect::<Vec<_>>()
            .join("; ");
        let more = if !unique.is_empty() {
            format!("; +{} more", unique.len())
        } else {
            String::new()
        };
        format!(" — failures: {shown}{more}")
    };

    if loaded == cards.len() {
        Check::connected(format!(
            "Jellyfin image sample: loaded {loaded}/{} posters",
            cards.len()
        ))
    } else if loaded > 0 {
        Check {
            state: CHECK_PARTIAL,
            detail: format!(
                "Jellyfin image sample: partial failure, loaded {loaded}/{} posters; {without_url} without URL{failure_note}",
                cards.len()
            ),
        }
    } else {
        Check::error(format!(
            "Jellyfin image sample: failed for 0/{} posters; {without_url} without URL{failure_note}",
            cards.len()
        ))
    }
}

fn fetch_any_fresh_jellyfin_image(
    urls: &[String],
    token: &str,
) -> Result<(), crate::thumbnails::ThumbnailFailure> {
    let mut last = crate::thumbnails::ThumbnailFailure::Request;
    for url in urls {
        match fetch_fresh_jellyfin_image(url, token) {
            Ok(()) => return Ok(()),
            Err(failure) => last = failure,
        }
    }
    Err(last)
}

fn fetch_fresh_jellyfin_image(
    url: &str,
    token: &str,
) -> Result<(), crate::thumbnails::ThumbnailFailure> {
    match crate::thumbnails::probe_image(url, Some(token)) {
        Ok(()) => Ok(()),
        Err(failure) => {
            // An item whose ImageTags no longer match the stored artwork fails
            // with 404 on the tagged URL but succeeds untagged; retry that
            // variant before reporting the tagged failure.
            let Ok(mut untagged) = url::Url::parse(url) else {
                return Err(failure);
            };
            let had_tag = untagged.query_pairs().any(|(key, _)| key == "tag");
            if !had_tag {
                return Err(failure);
            }
            let params = untagged
                .query_pairs()
                .filter(|(key, _)| key != "tag")
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect::<Vec<_>>();
            untagged.set_query(None);
            if !params.is_empty() {
                untagged.query_pairs_mut().extend_pairs(params);
            }
            match crate::thumbnails::probe_image(untagged.as_str(), Some(token)) {
                Ok(()) => Ok(()),
                // Report the original tagged failure: it is what the cards use.
                Err(_) => Err(failure),
            }
        }
    }
}

/// Probe the video service: reachability, one channel listing and one artwork
/// download. Returns the service check and the artwork check separately.
fn resolver_probe(resolver: &adult_hub_client::ResolverClient, base_url: &str) -> (Check, Check) {
    let categories = match resolver.get_categories(base_url) {
        Ok(categories) => categories,
        Err(error) => {
            return (
                Check::error(format!("Unavailable: {error}")),
                Check::skipped("Resolver image sample: skipped (no category listing)"),
            );
        }
    };
    let connection = Check::connected(format!("Connected (categories: {})", categories.len()));
    let Some(category) = categories.first() else {
        return (
            connection,
            Check::skipped("Resolver image sample: no categories available"),
        );
    };
    let videos = match resolver.get_category_videos(base_url, &category.id, 0, "latest", None) {
        Ok(videos) => videos,
        Err(error) => {
            return (
                connection,
                Check::error(format!("Resolver image sample: {error}")),
            );
        }
    };
    let Some(image_url) = videos
        .iter()
        .filter_map(|video| video.thumbnail.as_deref())
        .find_map(crate::thumbnails::resolve_resolver_thumbnail_url)
    else {
        return (
            connection,
            Check::skipped("Resolver image sample: no thumbnail in sample"),
        );
    };
    match crate::thumbnails::probe_image(&image_url, None) {
        Ok(()) => (connection, Check::connected("Resolver image sample: OK")),
        Err(failure) => (
            connection,
            Check::error(format!(
                "Resolver image sample: {stage} failed — {detail}",
                stage = failure.stage(),
                detail = failure.label()
            )),
        ),
    }
}

/// Ask the service for one real stream and range-probe it without downloading
/// the media. Kept separate from the availability checks: a server can answer
/// API requests while the actual video bytes are unreachable.
fn stream_access_probe(
    jellyfin: &JellyfinClient,
    session: Option<&adult_hub_storage::Session>,
    resolver_url: &str,
) -> Check {
    let Some(session) = session else {
        return Check::skipped("Stream probe skipped: no media server session");
    };
    let page = match jellyfin.get_latest_items(
        &session.server_url,
        &session.user_id,
        &session.access_token,
        None,
        1,
    ) {
        Ok(items) => items,
        Err(error) => return Check::error(format!("Stream probe skipped: {error}")),
    };
    let Some(item) = page.first() else {
        return Check::skipped("Stream probe skipped: no library item to probe");
    };
    let sampled = item.title.clone();
    let request = if uuid::Uuid::parse_str(&item.id).is_ok() {
        jellyfin
            .resolve_playback(
                &session.server_url,
                &session.user_id,
                &session.access_token,
                &item.id,
            )
            .map(|resolution| {
                (
                    resolution.stream_url,
                    Some(session.access_token.as_str()),
                    resolution.required_headers,
                )
            })
            .map_err(|error| error.to_string())
    } else if let Ok((url, _method)) = crate::session::PlaybackSessionController::resolve_stream_url(
        item,
        Some(&session.server_url),
        (!resolver_url.is_empty()).then_some(resolver_url),
        Some(&session.access_token),
    ) {
        Ok((url, None, Vec::new()))
    } else {
        Err(format!(
            "no stream source could be built for the sampled title \"{sampled}\""
        ))
    };
    let (url, token, headers) = match request {
        Ok(request) => request,
        Err(error) => return Check::error(redact_diagnostics(&error)),
    };
    match super::details::probe_remote_media_size(&url, token, &headers) {
        Some(bytes) => Check::connected(format!(
            "Stream reachable: {} ({})",
            adult_hub_client::format_media_size(bytes),
            sampled
        )),
        None => Check::error(format!(
            "Stream probe failed: no byte range for \"{sampled}\""
        )),
    }
}

/// Fold per-source checks into one surfaced state: an error dominates, a mix of
/// working and failing sources is partial, and nothing but skipped sources can
/// never report success.
fn merge_checks(parts: Vec<Check>) -> Check {
    let detail = parts
        .iter()
        .map(|part| part.detail.as_str())
        .filter(|detail| !detail.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let states = parts
        .iter()
        .map(|part| part.state)
        .collect::<Vec<&'static str>>();
    let any = |want: &str| states.contains(&want);
    let state = if states.is_empty() || any(CHECK_NOT_CHECKED) {
        CHECK_NOT_CHECKED
    } else if any(CHECK_SKIPPED) && states.iter().all(|state| *state == CHECK_SKIPPED) {
        CHECK_SKIPPED
    } else if any(CHECK_ERROR) && any(CHECK_CONNECTED) {
        CHECK_PARTIAL
    } else if any(CHECK_ERROR) {
        CHECK_ERROR
    } else if any(CHECK_PARTIAL) {
        CHECK_PARTIAL
    } else if any(CHECK_CONNECTED) {
        CHECK_CONNECTED
    } else {
        CHECK_NOT_CHECKED
    };
    Check { state, detail }
}

/// Run the whole diagnostic set on a background thread and publish one state
/// plus one explanation per check. Shared by the Settings "Test Connection"
/// button and the automatic check after saving a server address, so both
/// surfaces describe the same four checks: media server, video service, stream
/// access and artwork.
fn spawn_connection_diagnostics(
    weak: slint::Weak<MainWindow>,
    jellyfin: Arc<JellyfinClient>,
    resolver: Arc<adult_hub_client::ResolverClient>,
    session: Option<adult_hub_storage::Session>,
    resolver_url: String,
) {
    let Some(ui) = weak.upgrade() else { return };
    if ui.get_checking_connections() {
        return;
    }
    let (jellyfin_state, jellyfin_detail) = if session.is_some() {
        (CHECK_CHECKING, "Checking…".to_string())
    } else {
        (CHECK_SKIPPED, "Not signed in".to_string())
    };
    let (resolver_state, resolver_detail) = if resolver_url.is_empty() {
        (CHECK_SKIPPED, "No video service configured".to_string())
    } else {
        (CHECK_CHECKING, "Checking…".to_string())
    };
    ui.set_checking_connections(true);
    ui.set_jellyfin_connection_state(jellyfin_state.into());
    ui.set_jellyfin_connection_status(jellyfin_detail.into());
    ui.set_resolver_connection_state(resolver_state.into());
    ui.set_resolver_connection_status(resolver_detail.into());
    ui.set_artwork_connection_state(CHECK_CHECKING.into());
    ui.set_stream_connection_state(CHECK_CHECKING.into());
    ui.set_thumbnail_check_status("Checking artwork samples…".into());
    ui.set_stream_check_status("Checking stream access…".into());
    ui.set_thumbnail_check_failed(false);
    ui.set_connection_diagnostics(String::new().into());

    std::thread::spawn(move || {
        let server_check = match session.clone() {
            None => Check::skipped("Media server: not signed in"),
            Some(session) => {
                let probe_jellyfin = jellyfin.clone();
                match timed(20, move || {
                    probe_jellyfin
                        .get_user_views(
                            &session.server_url,
                            &session.user_id,
                            &session.access_token,
                        )
                        .map(|views| views.len())
                        .map_err(|error| error.to_string())
                }) {
                    Err(elapsed) => Check::error(format!("Media server: timed out ({elapsed})")),
                    Ok(Err(error)) => Check::error(format!("Media server: {error}")),
                    Ok(Ok(count)) => {
                        Check::connected(format!("Media server: connected ({count} libraries)"))
                    }
                }
            }
        };

        let service_check = if resolver_url.is_empty() {
            Check::skipped("Video service: not configured")
        } else {
            let probe_resolver = resolver.clone();
            let url = resolver_url.clone();
            match timed(20, move || {
                let resolver = probe_resolver;
                let url = url;
                resolver
                    .get_categories(&url)
                    .map(|categories| categories.len())
                    .map_err(|error| error.to_string())
            }) {
                Err(elapsed) => Check::error(format!("Video service: timed out ({elapsed})")),
                Ok(Err(error)) => Check::error(format!("Video service: {error}")),
                Ok(Ok(count)) => {
                    Check::connected(format!("Video service: connected ({count} categories)"))
                }
            }
        };

        let artwork_check = {
            let jellyfin_art = session.as_ref().map_or_else(
                || Check::skipped("Jellyfin artwork: not signed in"),
                |session| {
                    let probe_jellyfin = jellyfin.clone();
                    let session = session.clone();
                    match timed(25, move || {
                        jellyfin_thumbnail_probe(&probe_jellyfin, &session)
                    }) {
                        Err(elapsed) => {
                            Check::error(format!("Jellyfin artwork: timed out ({elapsed})"))
                        }
                        Ok(check) => check,
                    }
                },
            );
            let resolver_art = if resolver_url.is_empty() {
                Check::skipped("Video service artwork: not configured")
            } else {
                let probe_resolver = resolver.clone();
                let url = resolver_url.clone();
                match timed(25, move || resolver_probe(&probe_resolver, &url).1) {
                    Err(elapsed) => {
                        Check::error(format!("Video service artwork: timed out ({elapsed})"))
                    }
                    Ok(check) => check,
                }
            };
            merge_checks(vec![jellyfin_art, resolver_art])
        };

        let stream_check = match session.as_ref() {
            None => Check::skipped("Stream access: not signed in"),
            Some(session) => {
                let probe_jellyfin = jellyfin.clone();
                let session = session.clone();
                let url_for_probe = resolver_url.clone();
                match timed(30, move || {
                    stream_access_probe(&probe_jellyfin, Some(&session), &url_for_probe)
                }) {
                    Err(elapsed) => Check::error(format!("Stream access: timed out ({elapsed})")),
                    Ok(check) => check,
                }
            }
        };

        let checks = [server_check, service_check, stream_check, artwork_check];
        let diagnostics = checks
            .iter()
            .map(|check| format!("[{}] {}", check.state, redact_diagnostics(&check.detail)))
            .collect::<Vec<_>>()
            .join("\n");
        let overall_failed = checks.iter().any(|check| check.state == CHECK_ERROR);
        let overall_partial = checks.iter().any(|check| check.state == CHECK_PARTIAL);

        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut iter = checks.into_iter();
            let (server_check, service_check, stream_check, artwork_check) = (
                iter.next().unwrap(),
                iter.next().unwrap(),
                iter.next().unwrap(),
                iter.next().unwrap(),
            );
            ui.set_jellyfin_connection_state(server_check.state.into());
            ui.set_jellyfin_connection_status(server_check.detail.into());
            ui.set_resolver_connection_state(service_check.state.into());
            ui.set_resolver_connection_status(service_check.detail.into());
            ui.set_stream_connection_state(stream_check.state.into());
            ui.set_stream_check_status(stream_check.detail.into());
            ui.set_artwork_connection_state(artwork_check.state.into());
            ui.set_thumbnail_check_status(artwork_check.detail.into());
            ui.set_thumbnail_check_failed(matches!(
                artwork_check.state,
                CHECK_ERROR | CHECK_PARTIAL
            ));
            ui.set_connection_diagnostics(diagnostics.into());
            ui.set_checking_connections(false);
            ui.set_status(
                if overall_failed {
                    "Connection check finished with failures"
                } else if overall_partial {
                    "Connection check finished: some sources incomplete"
                } else {
                    "Connection check passed"
                }
                .into(),
            );
            ui.set_status_color(if overall_failed {
                slint::Color::from_rgb_u8(224, 108, 117)
            } else {
                slint::Color::from_rgb_u8(140, 200, 160)
            });
        });
    });
}

pub fn install_stage4_handlers(ui: &MainWindow, ctx: &AppContext) {
    let player = ctx.player.clone();
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let jellyfin = ctx.jellyfin.clone();
    let resolver = ctx.resolver.clone();
    let phash_coord = ctx.phash_coord.clone();
    let reporter = ctx.reporter.clone();
    let session_ctrl = ctx.session_ctrl.clone();
    let current_item = ctx.current_playing_item.clone();
    let cached_catalog = ctx.cached_catalog.clone();
    let autoplay_active = ctx.autoplay_active.clone();

    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_refresh_system_info(move || {
            if let Some(ui) = weak.upgrade() {
                crate::system_info::update_ui(&ui, player.as_deref());
                ui.set_status("System information refreshed".into());
                ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_confirm_dialog_accepted(move |action| {
            let Some(ui) = weak.upgrade() else { return };
            match action.as_str() {
                "search" => ui.invoke_clear_search_history(),
                "watch" => ui.invoke_clear_watch_history(),
                "progress" => ui.invoke_clear_progress_history(),
                "cache" => ui.invoke_clear_image_cache(),
                _ => {}
            }
        });
    }

    // 1. Emergency Panic Exit (Immediately hide, mute/stop, checkpoint, exit)
    {
        let weak = ui.as_weak();
        let player = player.clone();
        let session_ctrl = session_ctrl.clone();
        let current_item = current_item.clone();
        let reporter = reporter.clone();
        ui.on_panic_exit(move || {
            crate::diagnostics::GLOBAL_LOG.w(
                "Privacy",
                "Panic exit triggered; terminating process immediately",
            );
            if let Some(ui) = weak.upgrade() {
                let _ = ui.window().hide();
            }
            if let Some(player) = &player {
                let pos_ms = (player.position().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
                let dur_ms = (player.duration().unwrap_or(0.0) * 1000.0).max(0.0) as u64;
                let _ = player.set_muted(true);
                if let Some(item) = current_item.lock().unwrap().as_ref() {
                    session_ctrl.checkpoint_progress(item, pos_ms, dur_ms, false);
                }
                reporter.stop_session(pos_ms);
                let _ = player.stop();
            }
            std::process::exit(0);
        });
    }

    // 2. Clear Search History
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_clear_search_history(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_active_screen() == "settings" {
                let _ = local_store.clear_search_history();
                ui.set_recent_searches(
                    Rc::new(slint::VecModel::from(Vec::<slint::SharedString>::new())).into(),
                );
                ui.set_status("All search history cleared".into());
            } else {
                let ctx = super::browsing::active_search_context(&ui);
                let _ = local_store.clear_search_history_context(&ctx);
                super::browsing::refresh_recent_searches_for_context(&ui, &local_store);
                ui.set_status(format!("Search history cleared for {ctx}").into());
            }
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 3. Clear Resume Progress
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_clear_progress_history(move || {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.clear_all_progress();
            ui.set_status("Resume progress cleared".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            // Telegram resume data is local. Refresh the previous screen so
            // server-backed Jellyfin resume items remain visible and local
            // Telegram positions disappear from shelves and channel cards.
            refresh_content_after_filter_change(&ui);
        });
    }

    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let cached_catalog = cached_catalog.clone();
        ui.on_clear_watch_history(move || {
            let Some(ui) = weak.upgrade() else { return };
            if let Err(error) = local_store.clear_played_history() {
                ui.set_status(format!("Could not clear watch history: {error}").into());
                ui.set_status_color(slint::Color::from_rgb_u8(244, 67, 54));
                return;
            }
            ui.set_status("Watch history cleared".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            if ui.get_last_content_screen() == "home" {
                ui.invoke_refresh_home();
            } else if ui.get_active_category() == "Watch History" {
                set_catalog(&ui, Vec::new());
                cached_catalog.lock().unwrap().clear();
            }
        });
    }

    // 4. Save Resolver Base URL
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let phash_coord = phash_coord.clone();
        ui.on_save_resolver_url(move |url| {
            let Some(ui) = weak.upgrade() else { return };
            let mut trimmed = url.trim().trim_end_matches('/').to_string();
            if !trimmed.is_empty() {
                match ServiceUrlPolicy::inspect(&trimmed, false) {
                    ServiceUrlValidation::Valid(service_url) => {
                        if !super::request_cleartext_consent(
                            &ui,
                            &local_store,
                            &service_url.normalized,
                            "resolver-save",
                        ) {
                            return;
                        }
                        trimmed = service_url.normalized;
                    }
                    ServiceUrlValidation::Invalid(error) => {
                        show_error(&ui, error);
                        return;
                    }
                }
            }
            let _ = local_store.set_setting("resolver_base_url", &trimmed);
            let _ = local_store.set_setting("resolver_url", &trimmed);
            crate::thumbnails::set_resolver_base_url(&trimmed);
            let session = credential_store.get().ok().flatten();
            crate::thumbnails::set_resolver_auth_token(
                session
                    .as_ref()
                    .map(|session| session.access_token.as_str()),
            );
            phash_coord.update_auth(&trimmed, session.as_ref().map(|s| s.access_token.as_str()));
            ui.set_resolver_url_input(trimmed.clone().into());
            ui.set_saved_resolver_url(trimmed.into());
            ui.set_resolver_url_dirty(false);
            ui.set_status("Zaktualizowano adres serwera wideo".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            ui.invoke_test_resolver_connection();
        });
    }

    // 5. Test Connection (media server, video service, stream access, artwork)
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let resolver = resolver.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        ui.on_test_resolver_connection(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_checking_connections() {
                return;
            }
            // Only a configured video service is probed as such. Falling back to
            // the Jellyfin address here asked the media server for resolver
            // endpoints, which always failed and left "Status: Not checked"
            // even while playback worked.
            let mut url = ui
                .get_resolver_url_input()
                .trim()
                .trim_end_matches('/')
                .to_string();
            if url.is_empty() {
                url = local_store
                    .get_setting("resolver_base_url")
                    .ok()
                    .flatten()
                    .unwrap_or_default();
            }
            if !url.is_empty() {
                match ServiceUrlPolicy::inspect(&url, false) {
                    ServiceUrlValidation::Valid(service_url) => {
                        if !super::request_cleartext_consent(
                            &ui,
                            &local_store,
                            &service_url.normalized,
                            "resolver-test",
                        ) {
                            return;
                        }
                        url = service_url.normalized;
                    }
                    ServiceUrlValidation::Invalid(error) => {
                        show_error(&ui, error);
                        return;
                    }
                }
            }
            let session = credential_store.get().ok().flatten();
            spawn_connection_diagnostics(
                weak.clone(),
                jellyfin.clone(),
                resolver.clone(),
                session,
                url,
            );
        });
    }

    // Check both configured services and surface their results in Settings.
    {
        let weak = ui.as_weak();
        let jellyfin = jellyfin.clone();
        let resolver = resolver.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        ui.on_check_connections(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_checking_connections() {
                return;
            }
            let session = credential_store.get().ok().flatten();
            if let Some(session) = session.as_ref()
                && !super::request_cleartext_consent(
                    &ui,
                    &local_store,
                    &session.server_url,
                    "check-connections",
                )
            {
                return;
            }

            let mut resolver_url = ui
                .get_resolver_url_input()
                .trim()
                .trim_end_matches('/')
                .to_string();
            if resolver_url.is_empty() {
                resolver_url = local_store
                    .get_setting("resolver_base_url")
                    .ok()
                    .flatten()
                    .unwrap_or_default();
            }
            if !resolver_url.is_empty() {
                match ServiceUrlPolicy::inspect(&resolver_url, false) {
                    ServiceUrlValidation::Valid(service_url) => {
                        if !super::request_cleartext_consent(
                            &ui,
                            &local_store,
                            &service_url.normalized,
                            "check-connections",
                        ) {
                            return;
                        }
                        resolver_url = service_url.normalized;
                    }
                    ServiceUrlValidation::Invalid(error) => {
                        show_error(&ui, error);
                        return;
                    }
                }
            }

            spawn_connection_diagnostics(
                weak.clone(),
                jellyfin.clone(),
                resolver.clone(),
                session,
                resolver_url,
            );
        });
    }

    // Remove a saved HTTP trust grant and stop using its configured services.
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let credential_store = credential_store.clone();
        let phash_coord = phash_coord.clone();
        ui.on_revoke_cleartext_http_origin(move |origin| {
            let Some(ui) = weak.upgrade() else { return };
            let origin = origin.to_string();
            if let Err(error) = local_store.revoke_cleartext_origin(&origin) {
                show_error(&ui, format!("Could not revoke HTTP consent: {error}"));
                return;
            }

            let session = credential_store.get().ok().flatten();
            let active_server_uses_origin = session.as_ref().is_some_and(|session| {
                matches!(
                    ServiceUrlPolicy::inspect(&session.server_url, false),
                    ServiceUrlValidation::Valid(ref url) if url.origin == origin
                )
            });
            let resolver_url = ui.get_resolver_url_input().to_string();
            let active_resolver_uses_origin = matches!(
                ServiceUrlPolicy::inspect(&resolver_url, false),
                ServiceUrlValidation::Valid(url) if url.origin == origin
            );

            if active_resolver_uses_origin {
                let _ = local_store.set_setting("resolver_base_url", "");
                let _ = local_store.set_setting("resolver_url", "");
                crate::thumbnails::set_resolver_base_url("");
                crate::thumbnails::set_resolver_auth_token(
                    session
                        .as_ref()
                        .map(|session| session.access_token.as_str()),
                );
                ui.set_resolver_url_input("".into());
                if !active_server_uses_origin {
                    let session_url = session
                        .as_ref()
                        .map(|session| session.server_url.as_str())
                        .unwrap_or_default();
                    crate::thumbnails::set_resolver_base_url(session_url);
                    crate::thumbnails::set_resolver_auth_token(
                        session
                            .as_ref()
                            .map(|session| session.access_token.as_str()),
                    );
                    phash_coord.update_auth(
                        session_url,
                        session
                            .as_ref()
                            .map(|session| session.access_token.as_str()),
                    );
                }
            }
            if active_server_uses_origin {
                ui.invoke_perform_logout();
            }

            super::refresh_approved_http_origins(&ui, &local_store);
            ui.set_status(format!("HTTP trust revoked for {origin}").into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 6. Cycle Seek Step (10s, 15s, 30s)
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_cycle_seek_step(move || {
            let Some(ui) = weak.upgrade() else { return };
            let current = ui.get_seek_step_sec();
            let next = match current {
                5 => 10,
                10 => 15,
                15 => 30,
                _ => 5,
            };
            let _ = local_store.set_setting("seek_step_sec", &next.to_string());
            ui.set_seek_step_sec(next);
            ui.set_seek_step_label(format!("Step: {next}s").into());
        });
    }

    // 6b. Explicit Seek Step Selection (5s, 10s, 15s, 30s)
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_select_seek_step(move |step| {
            let Some(ui) = weak.upgrade() else { return };
            let step = step.clamp(5, 60);
            let _ = local_store.set_setting("seek_step_sec", &step.to_string());
            ui.set_seek_step_sec(step);
            ui.set_seek_step_label(format!("Step: {step}s").into());
        });
    }

    // 6c. Copy Build ID to clipboard
    {
        let weak = ui.as_weak();
        ui.on_copy_build_id(move |build_id| {
            let Some(ui) = weak.upgrade() else { return };
            match crate::copy_text_to_clipboard(build_id.as_str()) {
                Ok(()) => {
                    ui.set_toast_message("Build ID copied to clipboard".into());
                    ui.set_toast_type("info".into());
                    ui.set_toast_action_text("".into());
                    ui.set_toast_visible(true);
                }
                Err(err) => {
                    eprintln!("Failed to copy build ID: {err}");
                    ui.set_status("Failed to copy build ID to clipboard".into());
                }
            }
        });
    }

    // 7. Cycle the persistent thumbnail cache size
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_cycle_cache_limit(move || {
            let Some(ui) = weak.upgrade() else { return };
            let current = local_store
                .get_setting("cache_limit_mb")
                .ok()
                .flatten()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(1024);
            let next = match current {
                0 => 1024,
                1024 => 2048,
                2048 => 5120,
                5120 => 10240,
                10240 => 20480,
                20480 => 0,
                _ => 1024,
            };
            let _ = local_store.set_setting("cache_limit_mb", &next.to_string());
            crate::thumbnails::set_cache_limit_mb_async(next as usize);
            let limit_str = crate::thumbnails::format_cache_limit(next);
            ui.set_cache_limit_label(format!("Cache Limit: {limit_str}").into());
            ui.set_cache_limit_mb(next);
            let ui_weak = ui.as_weak();
            std::thread::spawn(move || {
                let used_bytes = crate::thumbnails::current_cache_size_bytes();
                let used_str = crate::thumbnails::format_cache_bytes(used_bytes);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_cache_usage_text(
                            format!("Used: {used_str} / Limit: {limit_str}").into(),
                        );
                    }
                });
            });
        });
    }

    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_set_cache_limit_mb(move |mb| {
            let Some(ui) = weak.upgrade() else { return };
            let mb = mb.max(0);
            let _ = local_store.set_setting("cache_limit_mb", &mb.to_string());
            crate::thumbnails::set_cache_limit_mb_async(mb as usize);
            let limit_str = crate::thumbnails::format_cache_limit(mb);
            ui.set_cache_limit_label(format!("Cache Limit: {limit_str}").into());
            ui.set_cache_limit_mb(mb);
            let ui_weak = ui.as_weak();
            std::thread::spawn(move || {
                let used_bytes = crate::thumbnails::current_cache_size_bytes();
                let used_str = crate::thumbnails::format_cache_bytes(used_bytes);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_cache_usage_text(
                            format!("Used: {used_str} / Limit: {limit_str}").into(),
                        );
                    }
                });
            });
        });
    }

    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_refresh_cache_size(move || {
            let Some(ui) = weak.upgrade() else { return };
            let current = local_store
                .get_setting("cache_limit_mb")
                .ok()
                .flatten()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(1024);
            let limit_str = crate::thumbnails::format_cache_limit(current);
            let ui_weak = ui.as_weak();
            std::thread::spawn(move || {
                let used_bytes = crate::thumbnails::current_cache_size_bytes();
                let used_str = crate::thumbnails::format_cache_bytes(used_bytes);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_cache_usage_text(
                            format!("Used: {used_str} / Limit: {limit_str}").into(),
                        );
                    }
                });
            });
        });
    }

    // 8. Toggle Low-RAM Mode
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_toggle_low_ram(move || {
            let Some(ui) = weak.upgrade() else { return };
            let active = !ui.get_low_ram_active();
            let _ = local_store.set_setting("low_ram_mode", if active { "true" } else { "false" });
            crate::thumbnails::set_low_ram_mode(active);
            ui.set_low_ram_active(active);
            ui.set_status(
                if active {
                    "Low-RAM mode enabled"
                } else {
                    "Low-RAM mode disabled"
                }
                .into(),
            );
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            // Settings is the active screen while this toggle is clicked. Refresh the
            // content screen the user came from so regenerated URLs use the new size.
            refresh_content_after_filter_change(&ui);
        });
    }

    // 9. Toggle Video Matching (pHash)
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_toggle_phash(move || {
            let Some(ui) = weak.upgrade() else { return };
            let active = !ui.get_phash_active();
            let _ =
                local_store.set_setting("phash_on_playback", if active { "true" } else { "false" });
            ui.set_phash_active(active);
            ui.set_status(
                if active {
                    "Video matching (pHash) enabled"
                } else {
                    "Video matching disabled"
                }
                .into(),
            );
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 10. Excluded Tags
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_add_excluded_tag(move |tag| {
            let Some(ui) = weak.upgrade() else { return };
            let tag = tag.trim().to_string();
            if !tag.is_empty() {
                let _ = local_store.exclude_tag(&tag);
                if let Ok(tags) = local_store.get_excluded_tags() {
                    let model = Rc::new(slint::VecModel::from(
                        tags.into_iter()
                            .map(slint::SharedString::from)
                            .collect::<Vec<_>>(),
                    ));
                    ui.set_excluded_tags_list(model.into());
                }
                ui.set_new_tag_input("".into());
                refresh_content_after_filter_change(&ui);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_remove_excluded_tag(move |tag| {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.remove_excluded_tag(&tag);
            if let Ok(tags) = local_store.get_excluded_tags() {
                let model = Rc::new(slint::VecModel::from(
                    tags.into_iter()
                        .map(slint::SharedString::from)
                        .collect::<Vec<_>>(),
                ));
                ui.set_excluded_tags_list(model.into());
            }
            refresh_content_after_filter_change(&ui);
        });
    }

    // 11. Unhide channels
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_unhide_channel(move |channel_id| {
            let Some(ui) = weak.upgrade() else { return };
            if let Err(error) = local_store.unhide_channel(&channel_id) {
                ui.set_status(format!("Could not unhide channel: {error}").into());
                ui.set_status_color(slint::Color::from_rgb_u8(244, 67, 54));
                return;
            }
            let channels = local_store.get_hidden_channels().unwrap_or_default();
            ui.set_hidden_channels_list(
                Rc::new(slint::VecModel::from(
                    channels
                        .into_iter()
                        .map(slint::SharedString::from)
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
            ui.invoke_refresh_channel_categories();
            ui.set_status("Channel unhidden".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 12. Unhide All Channels
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_unhide_all_channels(move || {
            let Some(ui) = weak.upgrade() else { return };
            let _ = local_store.clear_hidden_channels();
            let model = Rc::new(slint::VecModel::from(Vec::<slint::SharedString>::new()));
            ui.set_hidden_channels_list(model.into());
            ui.invoke_refresh_channel_categories();
            ui.set_status("All channels unhidden".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 12. Clear Image Cache
    {
        let weak = ui.as_weak();
        ui.on_clear_image_cache(move || {
            let Some(ui) = weak.upgrade() else { return };
            crate::thumbnails::clear_cache();
            match ui.get_last_content_screen().as_str() {
                "browse" => {
                    if ui.get_is_catalog_search_results() {
                        ui.invoke_search_catalog(ui.get_search_applied_query());
                    } else {
                        ui.invoke_filter_category(ui.get_active_category());
                    }
                }
                "channels" => {
                    let cat = ui.get_active_channel_category();
                    if !cat.is_empty() {
                        ui.invoke_select_channel_category(cat);
                    } else {
                        ui.invoke_refresh_channel_categories();
                    }
                }
                _ => ui.invoke_refresh_home(),
            }
            ui.set_status("Image cache cleared; reloading artwork…".into());
            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
        });
    }

    // 13. Export Configuration
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        ui.on_export_config(move || {
            let Some(ui) = weak.upgrade() else { return };
            match local_store.export_non_secret_config_json() {
                Ok(json) => {
                    let target_path = export_file_dialog("adult-hub-config.json", "json");
                    match std::fs::write(&target_path, json) {
                        Ok(_) => {
                            crate::diagnostics::GLOBAL_LOG
                                .i("Config", &format!("Config exported to {target_path}"));
                            ui.set_status(format!("Config exported to {target_path}").into());
                            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                        }
                        Err(e) => show_error(&ui, format!("Failed to write config: {e}")),
                    }
                }
                Err(e) => show_error(&ui, format!("Export error: {e}")),
            }
        });
    }

    // 14. Import Configuration
    {
        let weak = ui.as_weak();
        let local_store = local_store.clone();
        let phash_coord = phash_coord.clone();
        let credential_store = credential_store.clone();
        let autoplay_active = autoplay_active.clone();
        ui.on_import_config(move || {
            let Some(ui) = weak.upgrade() else { return };
            let import_path = pick_config_file();
            match import_path.and_then(|p| std::fs::read_to_string(&p).ok()) {
                Some(json) => match local_store.import_non_secret_config_json(&json) {
                    Ok(()) => {
                        crate::diagnostics::GLOBAL_LOG.i("Config", "Config imported successfully");
                        if let Ok(config) = local_store.export_non_secret_config() {
                            crate::thumbnails::set_cache_limit_mb(
                                config.cache_limit_mb.max(0) as usize
                            );
                            ui.set_seek_step_sec(config.seek_step_sec);
                            ui.set_seek_step_label(
                                format!("Step: {}s", config.seek_step_sec).into(),
                            );
                            ui.set_cache_limit_label(
                                format!(
                                    "Cache Limit: {}",
                                    crate::thumbnails::format_cache_limit(config.cache_limit_mb)
                                )
                                .into(),
                            );
                            ui.set_phash_active(config.phash_on_playback);
                            autoplay_active.store(config.autoplay_next, Ordering::SeqCst);
                            ui.set_autoplay_active(config.autoplay_next);
                            crate::thumbnails::set_low_ram_mode(config.low_ram_mode);
                            ui.set_low_ram_active(config.low_ram_mode);
                            if let Some(url) = config.resolver_base_url {
                                ui.set_resolver_url_input(url.into());
                            }
                            if let Some(lib_name) = config.selected_library_name {
                                ui.set_current_library(lib_name.into());
                            }
                            let tags_model = Rc::new(slint::VecModel::from(
                                config
                                    .excluded_tags
                                    .into_iter()
                                    .map(slint::SharedString::from)
                                    .collect::<Vec<_>>(),
                            ));
                            ui.set_excluded_tags_list(tags_model.into());
                            let channels_model = Rc::new(slint::VecModel::from(
                                config
                                    .hidden_channel_ids
                                    .into_iter()
                                    .map(slint::SharedString::from)
                                    .collect::<Vec<_>>(),
                            ));
                            ui.set_hidden_channels_list(channels_model.into());
                        }
                        ui.set_status("Configuration imported successfully".into());
                        ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                        let imported_resolver_url = local_store
                            .get_setting("resolver_base_url")
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        if !imported_resolver_url.is_empty() {
                            match ServiceUrlPolicy::inspect(&imported_resolver_url, false) {
                                ServiceUrlValidation::Valid(service_url)
                                    if service_url.uses_cleartext
                                        && !super::request_cleartext_consent(
                                            &ui,
                                            &local_store,
                                            &service_url.normalized,
                                            "resolver-save",
                                        ) =>
                                {
                                    let _ = local_store.set_setting("resolver_base_url", "");
                                    let _ = local_store.set_setting("resolver_url", "");
                                    ui.set_resolver_url_input(service_url.normalized.into());
                                    let session = credential_store.get().ok().flatten();
                                    phash_coord.update_auth(
                                        "",
                                        session
                                            .as_ref()
                                            .map(|session| session.access_token.as_str()),
                                    );
                                }
                                ServiceUrlValidation::Invalid(error) => {
                                    let _ = local_store.set_setting("resolver_base_url", "");
                                    let _ = local_store.set_setting("resolver_url", "");
                                    ui.set_resolver_url_input("".into());
                                    show_error(
                                        &ui,
                                        format!("Importowany adres serwera odrzucony: {error}"),
                                    );
                                }
                                _ => {}
                            }
                        }
                        let active_resolver_url = local_store
                            .get_setting("resolver_base_url")
                            .ok()
                            .flatten()
                            .filter(|url| !url.is_empty())
                            .or_else(|| {
                                credential_store
                                    .get()
                                    .ok()
                                    .flatten()
                                    .map(|session| session.server_url)
                            })
                            .unwrap_or_default();
                        crate::thumbnails::set_resolver_base_url(&active_resolver_url);
                        let active_token = credential_store
                            .get()
                            .ok()
                            .flatten()
                            .map(|session| session.access_token);
                        crate::thumbnails::set_resolver_auth_token(active_token.as_deref());
                    }
                    Err(err) => show_error(&ui, format!("Odrzucono import konfiguracji: {err}")),
                },
                None => {
                    show_error(&ui, "Nie wybrano pliku konfiguracji");
                }
            }
        });
    }

    // 15. Export Redacted Diagnostics
    {
        let weak = ui.as_weak();
        let player = player.clone();
        ui.on_export_diagnostics(move || {
            let Some(ui) = weak.upgrade() else { return };
            let target_path = export_file_dialog("adult-hub-diagnostics.txt", "txt");
            let info = crate::system_info::snapshot();
            let os = format!("{} {}", info.os_name, info.os_version);
            let processors = info.logical_processors.to_string();
            let total_memory = format!("{} MB", info.total_memory_mb);
            let available_memory = format!("{} MB", info.available_memory_mb);
            let vulkan_loader = info.vulkan_loader.clone();
            let decoder_support = match player
                .as_deref()
                .and_then(adult_hub_player::Player::common_video_decoder_support)
            {
                Some(support) => format!(
                    "H.264={}, HEVC={}, VP9={}, AV1={}",
                    support[0], support[1], support[2], support[3]
                ),
                None => "Unavailable".to_owned(),
            };
            let extra = [
                ("OS", os.as_str()),
                ("Processor", info.cpu_brand.as_str()),
                ("Logical processors", processors.as_str()),
                ("Total RAM", total_memory.as_str()),
                ("Available RAM", available_memory.as_str()),
                ("Vulkan loader", vulkan_loader.as_str()),
                ("libmpv decoder support", decoder_support.as_str()),
            ];
            let mut text =
                crate::diagnostics::GLOBAL_LOG.export_text(env!("CARGO_PKG_VERSION"), &extra);
            text.push_str("\n--- Startup log tail (redacted) ---\n");
            text.push_str(&crate::diagnostics::redacted_startup_log_tail(
                &crate::startup_log_path(),
                64 * 1024,
            ));
            let parent = std::path::Path::new(&target_path)
                .parent()
                .map(std::fs::create_dir_all);
            let result = match parent {
                Some(Err(e)) => Err(e),
                _ => std::fs::write(&target_path, text),
            };
            match result {
                Ok(()) => {
                    ui.set_status(format!("Redacted diagnostics exported to {target_path}").into());
                    ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                }
                Err(e) => show_error(&ui, format!("Diagnostics export failed: {e}")),
            }
        });
    }

    // 15b. Open the folder holding startup.log and store.db.
    {
        let weak = ui.as_weak();
        ui.on_open_logs_folder(move || {
            let Some(ui) = weak.upgrade() else { return };
            let dir = crate::startup_log_path()
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let opened = if cfg!(windows) {
                std::process::Command::new("explorer.exe")
                    .arg(&dir)
                    .spawn()
                    .is_ok()
            } else {
                std::process::Command::new("xdg-open")
                    .arg(&dir)
                    .spawn()
                    .is_ok()
            };
            if opened {
                crate::log_debug(&format!("logs folder opened: {dir}"));
                ui.set_status(format!("Logs folder opened: {dir}").into());
                ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
            } else {
                show_error(&ui, format!("Could not open the logs folder. Path: {dir}"));
            }
        });
    }

    // 16. List and select Jellyfin libraries.
    {
        let weak = ui.as_weak();
        let credential_store = credential_store.clone();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        ui.on_refresh_library_list(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(session) = credential_store.get().ok().flatten() else {
                ui.set_status("Sign in to load Jellyfin libraries".into());
                ui.set_status_color(slint::Color::from_rgb_u8(144, 152, 168));
                return;
            };
            ui.set_status("Loading Jellyfin libraries…".into());
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let local_store = local_store.clone();
            std::thread::spawn(move || {
                let selected_id = local_store
                    .get_setting("selected_library_id")
                    .ok()
                    .flatten();
                let result = jellyfin.get_user_views(
                    &session.server_url,
                    &session.user_id,
                    &session.access_token,
                );
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    match result {
                        Ok(views) => {
                            let options = views
                                .into_iter()
                                .map(|view| LibraryOptionData {
                                    selected: selected_id.as_deref() == Some(view.id.as_str()),
                                    id: view.id.into(),
                                    name: view.name.into(),
                                })
                                .collect::<Vec<_>>();
                            ui.set_library_options(Rc::new(slint::VecModel::from(options)).into());
                        }
                        Err(error) => show_error(&ui, format!("Could not load libraries: {error}")),
                    }
                })
                .ok();
            });
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_switch_library(move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_refresh_library_list();
            }
        });
    }
    {
        let weak = ui.as_weak();
        let credential_store = credential_store.clone();
        let jellyfin = jellyfin.clone();
        let local_store = local_store.clone();
        ui.on_select_library(move |library_id| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(session) = credential_store.get().ok().flatten() else {
                show_error(&ui, "Please log in to select a Jellyfin library");
                return;
            };
            let library_id = library_id.to_string();
            let weak = weak.clone();
            let jellyfin = jellyfin.clone();
            let local_store = local_store.clone();
            std::thread::spawn(move || {
                let result = jellyfin
                    .get_user_views(&session.server_url, &session.user_id, &session.access_token)
                    .map(|views| views.into_iter().find(|view| view.id == library_id));
                slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    match result {
                        Ok(Some(view)) => {
                            if let Err(error) = local_store
                                .set_setting("selected_library_id", &view.id)
                                .and_then(|_| {
                                    local_store.set_setting("selected_library_name", &view.name)
                                })
                            {
                                show_error(
                                    &ui,
                                    format!("Could not save selected library: {error}"),
                                );
                                return;
                            }
                            ui.set_current_library(view.name.into());
                            ui.set_active_category("Latest".into());
                            ui.set_search_query("".into());
                            ui.invoke_reset_catalog_text_filters();
                            ui.set_status("Active library changed".into());
                            ui.set_status_color(slint::Color::from_rgb_u8(140, 200, 160));
                            ui.invoke_refresh_library_list();
                            ui.invoke_refresh_home_categories();
                            ui.invoke_refresh_home();
                            if ui.get_active_screen() == "browse" {
                                ui.invoke_filter_category("Latest".into());
                            }
                        }
                        Ok(None) => show_error(&ui, "The selected library is no longer available"),
                        Err(error) => show_error(&ui, format!("Could not select library: {error}")),
                    }
                })
                .ok();
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_never_reveal_credentials() {
        // Regression: diagnostics strings are copied into the exported log and
        // shown in the expandable Settings section, so every credential form
        // Jellyfin uses has to be masked at the source.
        let text = "GET https://jf.local:8096/Items/1/Images/Primary?tag=abc&api_key=SECRET1 failed, headers Authorization: MediaBrowser Token=\"SECRET2\", X-Emby-Token: SECRET3";
        let redacted = redact_diagnostics(text);
        assert!(!redacted.contains("SECRET1"), "{redacted}");
        assert!(!redacted.contains("SECRET2"), "{redacted}");
        assert!(!redacted.contains("SECRET3"), "{redacted}");
        // The non-secret part of the diagnosis survives.
        assert!(redacted.contains("https://jf.local:8096/Items/1/Images/Primary"));
        assert!(redacted.contains("failed"), "{redacted}");
    }

    #[test]
    fn diagnostics_leave_plain_words_untouched() {
        let text = "Media server: connected (3 libraries), no token required";
        assert_eq!(redact_diagnostics(text), text);
    }

    #[test]
    fn diagnostics_redact_credentials_after_multibyte_text() {
        let text = "Błąd: stream failed · api_key=SECRET4";
        let redacted = redact_diagnostics(text);
        assert!(!redacted.contains("SECRET4"), "{redacted}");
        assert!(redacted.contains("Błąd: stream failed"), "{redacted}");
    }

    #[test]
    fn merged_checks_never_report_success_for_skipped_work() {
        let only_skips = merge_checks(vec![
            Check::skipped("Video service: not configured"),
            Check::skipped("Stream probe skipped: no session"),
        ]);
        assert_eq!(only_skips.state, CHECK_SKIPPED);

        let mixed = merge_checks(vec![
            Check::connected("Jellyfin image sample: loaded 5/5 posters"),
            Check::error("Resolver image sample: download failed"),
        ]);
        assert_eq!(mixed.state, CHECK_PARTIAL);
        assert!(mixed.detail.contains("loaded 5/5"));

        let all_fail = merge_checks(vec![
            Check::error("unreachable"),
            Check::skipped("not configured"),
        ]);
        assert_eq!(all_fail.state, CHECK_ERROR);

        let nothing_ran = merge_checks(Vec::new());
        assert_eq!(nothing_ran.state, CHECK_NOT_CHECKED);
    }
}
