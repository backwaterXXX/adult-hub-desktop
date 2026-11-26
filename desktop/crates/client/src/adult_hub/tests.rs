use super::*;
use crate::models::TvItem;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

fn spawn_mock_server<F>(handler: F) -> (String, thread::JoinHandle<()>)
where
    F: FnOnce(&str, &str, &str) -> (u16, Vec<(&'static str, &'static str)>, String)
        + Send
        + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let addr = listener.local_addr().unwrap();
    let url = format!("http://127.0.0.1:{}", addr.port());

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept connection");
        let mut raw_data = Vec::new();
        let mut buf = [0u8; 4096];
        let header_sep = b"\r\n\r\n";
        let mut body_start = None;
        let mut content_length = None;

        loop {
            let n = stream.read(&mut buf).expect("read request");
            if n == 0 {
                break;
            }
            raw_data.extend_from_slice(&buf[..n]);

            if body_start.is_none() {
                if let Some(pos) = raw_data.windows(4).position(|w| w == header_sep) {
                    body_start = Some(pos + 4);
                    let headers_str = String::from_utf8_lossy(&raw_data[..pos]);
                    for line in headers_str.lines() {
                        if let Some((k, v)) = line.split_once(':') {
                            if k.trim().eq_ignore_ascii_case("content-length") {
                                if let Ok(len) = v.trim().parse::<usize>() {
                                    content_length = Some(len);
                                }
                            }
                        }
                    }
                }
            }

            if let Some(start) = body_start {
                let needed_body = content_length.unwrap_or(0);
                if raw_data.len() >= start + needed_body {
                    break;
                }
            }
        }

        let (headers_part, body) = if let Some(start) = body_start {
            let h = String::from_utf8_lossy(&raw_data[..start - 4]).to_string();
            let b = String::from_utf8_lossy(&raw_data[start..]).to_string();
            (h, b)
        } else {
            (
                String::from_utf8_lossy(&raw_data).to_string(),
                String::new(),
            )
        };
        let first_line = headers_part.lines().next().unwrap_or("").to_string();

        let (status, headers, response_body) = handler(&first_line, &headers_part, &body);
        let status_text = match status {
            200 => "200 OK",
            206 => "206 Partial Content",
            302 => "302 Found",
            307 => "307 Temporary Redirect",
            308 => "308 Permanent Redirect",
            400 => "400 Bad Request",
            401 => "401 Unauthorized",
            403 => "403 Forbidden",
            404 => "404 Not Found",
            409 => "409 Conflict",
            416 => "416 Range Not Satisfiable",
            429 => "429 Too Many Requests",
            500 => "500 Internal Server Error",
            502 => "502 Bad Gateway",
            503 => "503 Service Unavailable",
            other => panic!("unmapped mock status code: {other}"),
        };

        let has_content_length = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-length"));
        let mut resp = format!("HTTP/1.1 {status_text}\r\nConnection: close\r\n");
        if !has_content_length {
            resp.push_str(&format!("Content-Length: {}\r\n", response_body.len()));
        }
        for (k, v) in headers {
            resp.push_str(&format!("{k}: {v}\r\n"));
        }
        resp.push_str("\r\n");
        resp.push_str(&response_body);

        stream.write_all(resp.as_bytes()).expect("write response");
        stream.flush().expect("flush response");
    });

    (url, handle)
}

#[test]
fn test_client_initialization_and_slash_safety() {
    let client = AdultHubClient::new("http://127.0.0.1:8090/").expect("client init");
    assert_eq!(client.base_url(), "http://127.0.0.1:8090");
    assert_eq!(client.origin(), "http://127.0.0.1:8090");

    // Leading and trailing slash variations must build the exact same URL
    let u1 = client.build_url("/api/v1/version").unwrap();
    let u2 = client.build_url("api/v1/version").unwrap();
    assert_eq!(u1.as_str(), "http://127.0.0.1:8090/api/v1/version");
    assert_eq!(u2.as_str(), "http://127.0.0.1:8090/api/v1/version");
}

#[test]
fn test_client_rejects_untrusted_origins() {
    let client = AdultHubClient::new("http://localhost:8090").expect("client init");

    // Attempting to resolve artwork with cross-origin URL must fail
    let hostile = "https://evil.com/api/v1/artwork/fake";
    let res = client.resolve_artwork_url(hostile);
    match res {
        Err(AdultHubError::OriginMismatch { expected, actual }) => {
            assert!(expected.contains("8090"));
            assert!(actual.contains("evil.com"));
        }
        _ => panic!("Expected OriginMismatch error"),
    }

    // Same origin artwork must resolve
    let local_art = "http://localhost:8090/api/v1/artwork/video/123/poster";
    assert_eq!(
        client.resolve_artwork_url(local_art).unwrap(),
        "http://localhost:8090/api/v1/artwork/video/123/poster"
    );

    // Relative artwork path must resolve against client origin
    let rel_art = "/api/v1/artwork/video/123/poster";
    assert_eq!(
        client.resolve_artwork_url(rel_art).unwrap(),
        "http://localhost:8090/api/v1/artwork/video/123/poster"
    );

    // Same-origin URL with embedded credentials / userinfo must be rejected
    assert!(matches!(
        client
            .resolve_artwork_url("http://user:pass@localhost:8090/api/v1/artwork/video/123/poster"),
        Err(AdultHubError::InvalidUrl(_))
    ));
    assert!(matches!(
        client.resolve_stream_url("http://user:pass@localhost:8090/api/v1/playback/stream/123"),
        Err(AdultHubError::InvalidUrl(_))
    ));
}

#[test]
fn test_artwork_url_construction_and_prefix_preservation() {
    let client = AdultHubClient::new("http://localhost:8090/custom_prefix").expect("client init");

    // Video artwork helper encodes video_id and preserves configured prefix
    let vid_url = client
        .get_video_artwork_url("telegram:-100123:456:789", "poster")
        .expect("video artwork url");
    assert_eq!(
        vid_url,
        "http://localhost:8090/custom_prefix/api/v1/artwork/video/telegram%3A-100123%3A456%3A789/poster"
    );

    // Performer artwork helper encodes performer_id and preserves prefix
    let perf_url = client
        .get_performer_artwork_url("stashdb:8c01d932-4412-4217-91a1-fbf56976f571")
        .expect("performer artwork url");
    assert_eq!(
        perf_url,
        "http://localhost:8090/custom_prefix/api/v1/artwork/performer/stashdb%3A8c01d932-4412-4217-91a1-fbf56976f571"
    );

    // Channel artwork helper encodes channel_id and preserves prefix
    let chan_url = client
        .get_channel_artwork_url("-100123456789")
        .expect("channel artwork url");
    assert_eq!(
        chan_url,
        "http://localhost:8090/custom_prefix/api/v1/artwork/channel/-100123456789"
    );

    // Reject unsupported artwork kind and traversal in kind
    assert!(matches!(
        client.get_video_artwork_url("telegram:1", "invalid_kind"),
        Err(AdultHubError::InvalidUrl(_))
    ));
    assert!(matches!(
        client.get_video_artwork_url("telegram:1", "../../etc/passwd"),
        Err(AdultHubError::InvalidUrl(_))
    ));

    // Reject dot/dot-dot segments in build_url
    assert!(matches!(
        client.build_url("api/v1/../etc/passwd"),
        Err(AdultHubError::InvalidUrl(_))
    ));
    assert!(matches!(
        client.build_url("api/v1/./poster"),
        Err(AdultHubError::InvalidUrl(_))
    ));
}

#[test]
fn test_artwork_wire_get_bytes_with_bearer_token() {
    let fake_bytes = b"test-fake-artwork-binary-bytes-data".to_vec();
    let (server_url, handle) = spawn_mock_server(move |line, headers, _body| {
        assert!(line.starts_with("GET /api/v1/artwork/video/v1/poster HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("authorization: bearer ah_test_token_art")
        );
        (
            200,
            vec![("Content-Type", "image/jpeg")],
            "test-fake-artwork-binary-bytes-data".to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url)
        .expect("init client")
        .with_token("ah_test_token_art");

    let bytes = client
        .get_artwork_bytes("/api/v1/artwork/video/v1/poster")
        .expect("fetch artwork bytes");
    handle.join().unwrap();
    assert_eq!(bytes, fake_bytes);
}

#[test]
fn test_artwork_wire_error_mappings() {
    // 404 Not Found mapping
    let (server_url_404, handle_404) = spawn_mock_server(|_line, _headers, _body| {
        (
            404,
            vec![("Content-Type", "application/json")],
            r#"{"error": "Artwork not found", "code": "ARTWORK_NOT_FOUND"}"#.to_string(),
        )
    });
    let client_404 = AdultHubClient::new(&server_url_404).expect("init client");
    let res_404 = client_404.get_artwork_bytes("/api/v1/artwork/video/absent/poster");
    handle_404.join().unwrap();
    assert!(matches!(res_404, Err(AdultHubError::NotFound(_))));

    // 401 Unauthorized mapping
    let (server_url_401, handle_401) = spawn_mock_server(|_line, _headers, _body| {
        (
            401,
            vec![("Content-Type", "application/json")],
            r#"{"error": "Bearer token required"}"#.to_string(),
        )
    });
    let client_401 = AdultHubClient::new(&server_url_401).expect("init client");
    let res_401 = client_401.get_artwork_bytes("/api/v1/artwork/video/v1/poster");
    handle_401.join().unwrap();
    assert!(matches!(
        res_401,
        Err(AdultHubError::Unauthorized { status: 401, .. })
    ));

    // PayloadTooLarge mapping when Content-Length exceeds MAX_IMAGE_BYTES (10 MB)
    let (server_url_large, handle_large) = spawn_mock_server(|_line, _headers, _body| {
        (
            200,
            vec![
                ("Content-Type", "image/jpeg"),
                ("Content-Length", "10485761"),
            ],
            "fake".to_string(),
        )
    });
    let client_large = AdultHubClient::new(&server_url_large).expect("init client");
    let res_large = client_large.get_artwork_bytes("/api/v1/artwork/video/v1/poster");
    handle_large.join().unwrap();
    assert!(matches!(
        res_large,
        Err(AdultHubError::PayloadTooLarge { limit: 10485760 })
    ));

    // Oversized error body (e.g. 100 KiB) is safely bounded before diagnostic extraction
    let large_error_text = "E".repeat(100 * 1024);
    let (server_url_err, handle_err) = spawn_mock_server(move |_line, _headers, _body| {
        (500, vec![("Content-Type", "text/plain")], large_error_text)
    });
    let client_err = AdultHubClient::new(&server_url_err).expect("init client");
    let res_err = client_err.get_artwork_bytes("/api/v1/artwork/video/v1/poster");
    handle_err.join().unwrap();
    match res_err {
        Err(AdultHubError::Api {
            status, message, ..
        }) => {
            assert_eq!(status, 500);
            assert_eq!(message.len(), 250);
            assert!(message.starts_with("EEE"));
        }
        other => panic!("expected Api error with bounded message, got {other:?}"),
    }

    // Body I/O failure on success response is mapped to AdultHubError::Io (not InvalidUrl)
    let listener_ok = std::net::TcpListener::bind("127.0.0.1:0").expect("bind tcp");
    let port_ok = listener_ok.local_addr().unwrap().port();
    let handle_abort_ok = std::thread::spawn(move || {
        let (mut stream, _) = listener_ok.accept().expect("accept tcp");
        let mut buf = [0u8; 1024];
        let _ = std::io::Read::read(&mut stream, &mut buf);
        let partial = "HTTP/1.1 200 OK\r\nContent-Length: 5000\r\nConnection: close\r\n\r\npart";
        let _ = std::io::Write::write_all(&mut stream, partial.as_bytes());
        let _ = std::io::Write::flush(&mut stream);
        drop(stream);
    });
    let client_abort_ok =
        AdultHubClient::new(&format!("http://127.0.0.1:{port_ok}")).expect("init client");
    let res_abort_ok = client_abort_ok.get_artwork_bytes("/api/v1/artwork/video/v1/poster");
    handle_abort_ok.join().unwrap();
    match res_abort_ok {
        Err(AdultHubError::Io(_)) => {}
        other => panic!("expected AdultHubError::Io, got {other:?}"),
    }

    // Body I/O failure on error response is mapped to AdultHubError::Io (not InvalidUrl)
    let listener_err = std::net::TcpListener::bind("127.0.0.1:0").expect("bind tcp");
    let port_err = listener_err.local_addr().unwrap().port();
    let handle_abort_err = std::thread::spawn(move || {
        let (mut stream, _) = listener_err.accept().expect("accept tcp");
        let mut buf = [0u8; 1024];
        let _ = std::io::Read::read(&mut stream, &mut buf);
        let partial = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5000\r\nConnection: close\r\n\r\npart";
        let _ = std::io::Write::write_all(&mut stream, partial.as_bytes());
        let _ = std::io::Write::flush(&mut stream);
        drop(stream);
    });
    let client_abort_err =
        AdultHubClient::new(&format!("http://127.0.0.1:{port_err}")).expect("init client");
    let res_abort_err = client_abort_err.get_artwork_bytes("/api/v1/artwork/video/v1/poster");
    handle_abort_err.join().unwrap();
    match res_abort_err {
        Err(AdultHubError::Io(_)) => {}
        other => panic!("expected AdultHubError::Io, got {other:?}"),
    }
}

#[test]
fn test_sensitive_credentials_and_tokens_are_redacted() {
    let req = LoginRequest {
        username: "alice",
        password: "super_secret_password_123",
        device_name: Some("Desktop"),
        device_id: Some("desk-1"),
    };
    let req_debug = format!("{req:?}");
    assert!(!req_debug.contains("super_secret_password_123"));
    assert!(req_debug.contains("[REDACTED]"));
    assert!(req_debug.contains("alice"));

    let token_resp = AuthTokenResponse {
        access_token: "ah_tok_secret_access_123".into(),
        token_type: "Bearer".into(),
        expires_in: 900,
        refresh_token: "ah_ref_secret_refresh_456".into(),
        user: ClientUserDto {
            id: "usr_1".into(),
            username: "alice".into(),
        },
    };
    let token_debug = format!("{token_resp:?}");
    assert!(!token_debug.contains("ah_tok_secret_access_123"));
    assert!(!token_debug.contains("ah_ref_secret_refresh_456"));
    assert!(token_debug.contains("[REDACTED]"));

    let refresh_resp = AuthRefreshResponse {
        access_token: "ah_tok_secret_access_789".into(),
        token_type: "Bearer".into(),
        expires_in: 900,
        refresh_token: "ah_ref_secret_refresh_999".into(),
    };
    let refresh_resp_debug = format!("{refresh_resp:?}");
    assert!(!refresh_resp_debug.contains("ah_tok_secret_access_789"));
    assert!(!refresh_resp_debug.contains("ah_ref_secret_refresh_999"));
    assert!(refresh_resp_debug.contains("[REDACTED]"));

    let refresh_req = RefreshTokenRequest {
        refresh_token: "ah_ref_secret_refresh_456",
    };
    let refresh_debug = format!("{refresh_req:?}");
    assert!(!refresh_debug.contains("ah_ref_secret_refresh_456"));
    assert!(refresh_debug.contains("[REDACTED]"));

    let client = AdultHubClient::new("http://127.0.0.1:8090")
        .unwrap()
        .with_token("ah_tok_super_secret");
    let client_debug = format!("{client:?}");
    assert!(!client_debug.contains("ah_tok_super_secret"));
    assert!(client_debug.contains("has_token: true"));
}

#[test]
fn test_deserializes_version_discovery() {
    let raw = r#"{
            "api_version": "1.0.0",
            "supported_versions": ["1.0.0"],
            "server_version": "0.4.15",
            "status": "ok",
            "capabilities": {
                "desktop_api": true,
                "direct_play": false,
                "user_state": false,
                "tv_pairing": false,
                "transcoding": false
            },
            "endpoints": {
                "auth": "/api/v1/auth",
                "catalog": "/api/v1/catalog"
            }
        }"#;

    let dto: ApiVersionResponse = serde_json::from_str(raw).expect("parse discovery");
    assert_eq!(dto.api_version, "1.0.0");
    assert!(dto.is_ok());
    assert!(dto.supports_version("1.0.0"));
    assert!(!dto.supports_version("2.0.0"));
    assert!(dto.has_capability("desktop_api"));
    assert!(!dto.has_capability("direct_play"));
    assert_eq!(
        dto.endpoints.get("auth").map(|s| s.as_str()),
        Some("/api/v1/auth")
    );
}

#[test]
fn test_deserializes_auth_token_response() {
    let raw = r#"{
            "access_token": "ah_tok_9f8a8b12c4d5e6f7a8b9c0d1e2f3a4b5",
            "token_type": "Bearer",
            "expires_in": 900,
            "refresh_token": "ah_ref_3d2c1b0a9f8e7d6c5b4a3a2b1c0d",
            "user": {
                "id": "usr_01h8a9bc4d5e6f7a8b9c0d1e2f",
                "username": "viewer"
            }
        }"#;

    let dto: AuthTokenResponse = serde_json::from_str(raw).expect("parse auth");
    assert_eq!(dto.access_token, "ah_tok_9f8a8b12c4d5e6f7a8b9c0d1e2f3a4b5");
    assert_eq!(dto.refresh_token, "ah_ref_3d2c1b0a9f8e7d6c5b4a3a2b1c0d");
    assert_eq!(dto.user.username, "viewer");
    assert_eq!(dto.expires_in, 900);
}

#[test]
fn test_deserializes_auth_refresh_response() {
    let raw = r#"{
            "access_token": "ah_tok_rotated_access",
            "token_type": "Bearer",
            "expires_in": 900,
            "refresh_token": "ah_ref_rotated_refresh"
        }"#;

    let dto: AuthRefreshResponse = serde_json::from_str(raw).expect("parse refresh response");
    assert_eq!(dto.access_token, "ah_tok_rotated_access");
    assert_eq!(dto.token_type, "Bearer");
    assert_eq!(dto.expires_in, 900);
    assert_eq!(dto.refresh_token, "ah_ref_rotated_refresh");
}

#[test]
fn test_deserializes_catalog_videos_and_detail() {
    let raw = r#"{
            "items": [
                {
                    "id": "telegram:-100123456789:456:789012",
                    "title": "Summer Heat Episode 1",
                    "duration_seconds": 1845,
                    "release_date": "2026-05-12",
                    "date_added": "2026-10-01T14:30:00Z",
                    "studio": "Dragon Media",
                    "plot": "Outdoor action scene.",
                    "categories": ["Twinks", "Bareback"],
                    "tags": ["Pool", "Outdoor"],
                    "performers": [
                        {
                            "id": "stashdb:8c01d932-4412-4217-91a1-fbf56976f571",
                            "name": "Alex Frost",
                            "photo_url": "/api/v1/artwork/performer/stashdb:8c01d932-4412-4217-91a1-fbf56976f571"
                        }
                    ],
                    "poster_url": "/api/v1/artwork/video/telegram:-100123456789:456:789012/poster",
                    "backdrop_url": "/api/v1/artwork/video/telegram:-100123456789:456:789012/backdrop",
                    "file_size_bytes": 1450234890,
                    "width": 1920,
                    "height": 1080,
                    "is_hd": true,
                    "user_state": {
                        "is_favorite": true,
                        "position_seconds": 450,
                        "played_percentage": 24.38,
                        "is_played": false
                    }
                }
            ],
            "total_count": 1,
            "offset": 0,
            "limit": 50,
            "has_more": false
        }"#;

    let page: PaginatedVideosResponse = serde_json::from_str(raw).expect("parse videos page");
    assert_eq!(page.total_count, 1);
    assert_eq!(page.items.len(), 1);
    let item = &page.items[0];
    assert_eq!(item.id, "telegram:-100123456789:456:789012");
    assert_eq!(item.title, "Summer Heat Episode 1");
    assert_eq!(item.duration_seconds, 1845);
    assert_eq!(item.performers.len(), 1);
    assert_eq!(item.performers[0].name, "Alex Frost");
    assert_eq!(
        item.user_state.as_ref().and_then(|s| s.is_favorite),
        Some(true)
    );
    assert_eq!(
        item.user_state.as_ref().and_then(|s| s.position_seconds),
        Some(450)
    );
}

#[test]
fn test_deserializes_catalog_channels() {
    let raw = r#"{
            "items": [
                {
                    "id": "-100123456789",
                    "title": "Studio Channel Alpha",
                    "description": "Primary release channel.",
                    "video_count": 342,
                    "icon_url": "/api/v1/artwork/channel/-100123456789",
                    "is_enabled": true
                }
            ],
            "total_count": 1,
            "offset": 0,
            "limit": 50,
            "has_more": false
        }"#;

    let channels: PaginatedChannelsResponse = serde_json::from_str(raw).expect("parse channels");
    assert_eq!(channels.total_count, 1);
    assert_eq!(channels.items.len(), 1);
    assert_eq!(channels.items[0].id, "-100123456789");
    assert_eq!(channels.items[0].video_count, 342);
    assert!(channels.items[0].is_enabled);
}

#[test]
fn test_deserializes_rfc7807_problem_details() {
    let raw = r#"{
            "type": "https://adulthub.media/errors/video-source-unavailable",
            "title": "Video Source Unavailable",
            "status": 502,
            "detail": "None of the registered sources could be reached.",
            "code": "SOURCE_UNAVAILABLE"
        }"#;

    let prob: ProblemDetails = serde_json::from_str(raw).expect("parse problem details");
    assert_eq!(prob.title.as_deref(), Some("Video Source Unavailable"));
    assert_eq!(
        prob.detail.as_deref(),
        Some("None of the registered sources could be reached.")
    );
    assert_eq!(prob.code.as_deref(), Some("SOURCE_UNAVAILABLE"));
}

// -----------------------------------------------------------------------
// Wire Integration Tests with Real HTTP Socket
// -----------------------------------------------------------------------

#[test]
fn test_wire_discover_version() {
    let (server_url, handle) = spawn_mock_server(|line, _headers, _body| {
        assert!(line.starts_with("GET /api/v1/version HTTP/1.1"));
        let body = r#"{
                "api_version": "1.0.0",
                "supported_versions": ["1.0.0"],
                "server_version": "0.4.15",
                "status": "ok",
                "capabilities": {"desktop_api": true},
                "endpoints": {"auth": "/api/v1/auth", "catalog": "/api/v1/catalog"}
            }"#;
        (
            200,
            vec![("Content-Type", "application/json")],
            body.to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url).expect("init client");
    let res = client.discover_version().expect("discover version");
    handle.join().unwrap();

    assert_eq!(res.api_version, "1.0.0");
    assert_eq!(res.server_version, "0.4.15");
    assert!(res.has_capability("desktop_api"));
}

#[test]
fn test_wire_login_and_token_storage() {
    let (server_url, handle) = spawn_mock_server(|line, headers, body| {
        assert!(line.starts_with("POST /api/v1/auth/login HTTP/1.1"));
        assert!(headers.contains("content-type: application/json"));
        assert!(body.contains("\"username\":\"bob\""));
        assert!(body.contains("\"password\":\"secretpass\""));
        assert!(body.contains("\"device_name\":\"Living Room\""));

        let resp_body = r#"{
                "access_token": "ah_tok_12345",
                "token_type": "Bearer",
                "expires_in": 900,
                "refresh_token": "ah_ref_67890",
                "user": {
                    "id": "usr_99",
                    "username": "bob"
                }
            }"#;
        (
            200,
            vec![("Content-Type", "application/json")],
            resp_body.to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url).expect("init client");
    assert!(client.token().is_none());

    let res = client
        .login_with_device("bob", "secretpass", Some("Living Room"), Some("dev-1"))
        .expect("login");
    handle.join().unwrap();

    assert_eq!(res.access_token, "ah_tok_12345");
    assert_eq!(client.token().as_deref(), Some("ah_tok_12345"));
}

#[test]
fn test_wire_refresh_and_revoke_token() {
    // Test refresh
    let (server_url, handle) = spawn_mock_server(|line, _headers, body| {
        assert!(line.starts_with("POST /api/v1/auth/refresh HTTP/1.1"));
        assert!(body.contains("\"refresh_token\":\"ah_ref_old\""));
        let resp_body = r#"{
                "access_token": "ah_tok_new",
                "token_type": "Bearer",
                "expires_in": 900,
                "refresh_token": "ah_ref_new"
            }"#;
        (
            200,
            vec![("Content-Type", "application/json")],
            resp_body.to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url).expect("init client");
    let refresh_res = client.refresh_token("ah_ref_old").expect("refresh");
    handle.join().unwrap();

    assert_eq!(refresh_res.access_token, "ah_tok_new");
    assert_eq!(refresh_res.refresh_token, "ah_ref_new");
    assert_eq!(refresh_res.expires_in, 900);
    assert_eq!(client.token().as_deref(), Some("ah_tok_new"));

    // Test revoke clears active token
    let (server_url2, handle2) = spawn_mock_server(|line, _headers, body| {
        assert!(line.starts_with("POST /api/v1/auth/revoke HTTP/1.1"));
        assert!(body.contains("\"refresh_token\":\"ah_ref_new\""));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{"revoked": true}"#.to_string(),
        )
    });

    let client2 = AdultHubClient::new(&server_url2)
        .expect("init client")
        .with_token("ah_tok_new");
    assert_eq!(client2.token().as_deref(), Some("ah_tok_new"));

    let revoke_res = client2.revoke_token("ah_ref_new").expect("revoke");
    handle2.join().unwrap();

    assert!(revoke_res.revoked);
    assert!(client2.token().is_none());
}

#[test]
fn test_wire_regression_refresh_exact_real_api_response() {
    // Backend AuthRefreshResponseDTO strictly omits `user`.
    // Client must deserialize AuthRefreshResponse and update stored bearer token.
    let (server_url, handle) = spawn_mock_server(|line, _headers, body| {
        assert!(line.starts_with("POST /api/v1/auth/refresh HTTP/1.1"));
        assert!(body.contains("\"refresh_token\":\"ah_ref_old_rotated\""));
        let exact_real_backend_json = r#"{
                "access_token": "ah_tok_new_rotated",
                "token_type": "Bearer",
                "expires_in": 900,
                "refresh_token": "ah_ref_new_rotated"
            }"#;
        (
            200,
            vec![("Content-Type", "application/json")],
            exact_real_backend_json.to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url)
        .expect("init client")
        .with_token("ah_tok_old_token");

    assert_eq!(client.token().as_deref(), Some("ah_tok_old_token"));

    let refresh_res = client.refresh_token("ah_ref_old_rotated").expect("refresh");
    handle.join().unwrap();

    assert_eq!(refresh_res.access_token, "ah_tok_new_rotated");
    assert_eq!(refresh_res.refresh_token, "ah_ref_new_rotated");
    assert_eq!(refresh_res.token_type, "Bearer");
    assert_eq!(refresh_res.expires_in, 900);

    // Stored bearer token must be updated to the newly rotated access token
    assert_eq!(client.token().as_deref(), Some("ah_tok_new_rotated"));
    assert_eq!(
        client.auth_header_value().as_deref(),
        Some("Bearer ah_tok_new_rotated")
    );
}

#[test]
fn test_wire_catalog_videos_and_detail_with_bearer_token() {
    // Browse videos
    let (server_url, handle) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("GET /api/v1/catalog/videos?offset=10&limit=25&sort=duration&channel_id=-100&q=pool HTTP/1.1"));
        assert!(headers.contains("authorization: Bearer my_active_token"));

        let resp_body = r#"{
                "items": [
                    {
                        "id": "vid_1",
                        "title": "Pool Party",
                        "duration_seconds": 1200,
                        "categories": ["Party"],
                        "tags": ["Pool"],
                        "performers": []
                    }
                ],
                "total_count": 100,
                "offset": 10,
                "limit": 25,
                "has_more": true
            }"#;
        (
            200,
            vec![("Content-Type", "application/json")],
            resp_body.to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url)
        .expect("init")
        .with_token("my_active_token");

    let q = CatalogVideosQuery {
        offset: Some(10),
        limit: Some(25),
        sort: Some("duration"),
        channel_id: Some("-100"),
        q: Some("pool"),
        ..Default::default()
    };
    let res = client.get_catalog_videos(&q).expect("query videos");
    handle.join().unwrap();

    assert_eq!(res.total_count, 100);
    assert_eq!(res.items.len(), 1);
    assert_eq!(res.items[0].title, "Pool Party");

    // Detail video
    let (server_url2, handle2) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("GET /api/v1/catalog/videos/telegram%3A-100%3A456 HTTP/1.1"));
        assert!(headers.contains("authorization: Bearer my_active_token"));

        let resp_body = r#"{
                "id": "telegram:-100:456",
                "title": "Summer Heat",
                "duration_seconds": 1800,
                "performers": []
            }"#;
        (
            200,
            vec![("Content-Type", "application/json")],
            resp_body.to_string(),
        )
    });

    let client2 = AdultHubClient::new(&server_url2)
        .expect("init")
        .with_token("my_active_token");
    let detail = client2
        .get_catalog_video_detail("telegram:-100:456")
        .expect("video detail");
    handle2.join().unwrap();

    assert_eq!(detail.id, "telegram:-100:456");
    assert_eq!(detail.title, "Summer Heat");
}

#[test]
fn test_wire_catalog_channels_and_detail() {
    let (server_url, handle) = spawn_mock_server(|line, headers, _body| {
        assert!(
            line.starts_with("GET /api/v1/catalog/channels?limit=10&sort=video_count HTTP/1.1")
        );
        assert!(headers.contains("authorization: Bearer tok_channels"));

        let resp_body = r#"{
                "items": [
                    {
                        "id": "-100123",
                        "title": "Alpha Channel",
                        "video_count": 55,
                        "is_enabled": true
                    }
                ],
                "total_count": 1,
                "offset": 0,
                "limit": 10,
                "has_more": false
            }"#;
        (
            200,
            vec![("Content-Type", "application/json")],
            resp_body.to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url)
        .expect("init")
        .with_token("tok_channels");

    let q = CatalogChannelsQuery {
        limit: Some(10),
        sort: Some("video_count"),
        ..Default::default()
    };
    let channels = client.get_catalog_channels(&q).expect("query channels");
    handle.join().unwrap();

    assert_eq!(channels.items.len(), 1);
    assert_eq!(channels.items[0].id, "-100123");
    assert_eq!(channels.items[0].video_count, 55);

    // Detail channel
    let (server_url2, handle2) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("GET /api/v1/catalog/channels/-100123 HTTP/1.1"));
        assert!(headers.contains("authorization: Bearer tok_channels"));

        let resp_body = r#"{
                "id": "-100123",
                "title": "Alpha Channel",
                "video_count": 55,
                "is_enabled": true
            }"#;
        (
            200,
            vec![("Content-Type", "application/json")],
            resp_body.to_string(),
        )
    });

    let client2 = AdultHubClient::new(&server_url2)
        .expect("init")
        .with_token("tok_channels");
    let ch = client2
        .get_catalog_channel_detail("-100123")
        .expect("channel detail");
    handle2.join().unwrap();

    assert_eq!(ch.id, "-100123");
    assert_eq!(ch.video_count, 55);
}

#[test]
fn test_wire_refuses_to_follow_untrusted_redirects() {
    let (server_url, handle) = spawn_mock_server(|line, _headers, _body| {
        assert!(line.starts_with("GET /api/v1/version HTTP/1.1"));
        (
            302,
            vec![("Location", "https://untrusted-site.com/hijack")],
            "Redirecting".to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url).expect("init client");
    let res = client.discover_version();
    handle.join().unwrap();

    // With redirect policy none, 302 is returned as an Api error or status rather than
    // forwarding headers to the redirect location.
    match res {
        Err(AdultHubError::Api { status, .. }) => {
            assert_eq!(status, 302);
        }
        Err(e) => panic!("Expected Api 302, got: {e:?}"),
        Ok(_) => panic!("Expected error on 302, got success"),
    }
}

#[test]
fn test_wire_error_mapping_401_404_429() {
    // Test 401 unauthorized
    let (server_url, handle) = spawn_mock_server(|_line, _headers, _body| {
        (
                401,
                vec![("Content-Type", "application/problem+json")],
                r#"{"title": "Unauthorized", "detail": "Invalid or expired access token", "status": 401}"#.to_string(),
            )
    });

    let client = AdultHubClient::new(&server_url).expect("init client");
    let err = client.get_catalog_videos(&Default::default()).unwrap_err();
    handle.join().unwrap();

    assert!(err.is_unauthorized());
    match err {
        AdultHubError::Unauthorized { status, message } => {
            assert_eq!(status, 401);
            assert!(message.contains("Invalid or expired access token"));
        }
        _ => panic!("Expected Unauthorized variant"),
    }

    // Test 404 not found
    let (server_url2, handle2) = spawn_mock_server(|_line, _headers, _body| {
        (
            404,
            vec![("Content-Type", "application/json")],
            r#"{"error": "Video not found"}"#.to_string(),
        )
    });

    let client2 = AdultHubClient::new(&server_url2).expect("init client");
    let err2 = client2.get_catalog_video_detail("missing_vid").unwrap_err();
    handle2.join().unwrap();

    match err2 {
        AdultHubError::NotFound(msg) => {
            assert!(msg.contains("Video not found"));
        }
        _ => panic!("Expected NotFound variant"),
    }

    // Test 429 rate limited
    let (server_url3, handle3) = spawn_mock_server(|_line, _headers, _body| {
        (
            429,
            vec![("Content-Type", "application/json")],
            r#"{"error": "Rate limit exceeded"}"#.to_string(),
        )
    });

    let client3 = AdultHubClient::new(&server_url3).expect("init client");
    let err3 = client3.login("user", "pass").unwrap_err();
    handle3.join().unwrap();

    assert!(err3.is_rate_limited());
    match err3 {
        AdultHubError::RateLimited(msg) => {
            assert!(msg.contains("Rate limit exceeded"));
        }
        _ => panic!("Expected RateLimited variant"),
    }
}

#[test]
fn test_deserializes_playback_capabilities() {
    let raw = r#"{
            "streaming_protocols": ["http_range_rfc7233"],
            "supported_containers": ["video/mp4", "video/webm", "video/x-matroska"],
            "seek_alignment_bytes": 1048576,
            "pinned_representation_required": true,
            "supports_head_requests": true
        }"#;

    let caps: PlaybackCapabilities = serde_json::from_str(raw).expect("parse capabilities");
    assert_eq!(caps.streaming_protocols, vec!["http_range_rfc7233"]);
    assert!(caps.pinned_representation_required);
    assert!(caps.supports_head_requests);
    assert_eq!(caps.seek_alignment_bytes, Some(1048576));
}

#[test]
fn test_deserializes_playback_descriptor() {
    let raw = r#"{
            "video_id": "telegram:canonical_vid_1",
            "stream_path": "/api/v1/playback/stream/telegram:canonical_vid_1",
            "mime_type": "video/mp4",
            "duration_seconds": 3600.0,
            "file_size_bytes": 1450234890,
            "width": 1920,
            "height": 1080,
            "renditions": [
                {
                    "id": "original",
                    "label": "Original Direct Play (1080p)",
                    "mime_type": "video/mp4",
                    "file_size_bytes": 1450234890,
                    "width": 1920,
                    "height": 1080,
                    "is_default": true,
                    "stream_path": "/api/v1/playback/stream/telegram:canonical_vid_1"
                },
                {
                    "id": "alt_720",
                    "label": "Telegram Alternate Stream (720p)",
                    "mime_type": "video/mp4",
                    "file_size_bytes": 720112400,
                    "width": 1280,
                    "height": 720,
                    "is_default": false,
                    "stream_path": "/api/v1/playback/stream/telegram:canonical_vid_1?rendition_id=alt_720"
                }
            ]
        }"#;

    let desc: PlaybackDescriptorDto = serde_json::from_str(raw).expect("parse descriptor");
    assert_eq!(desc.video_id, "telegram:canonical_vid_1");
    assert_eq!(
        desc.default_stream_path(),
        Some("/api/v1/playback/stream/telegram:canonical_vid_1")
    );
    assert_eq!(desc.renditions.len(), 2);

    let alt = desc.rendition_by_id("alt_720").expect("find alt");
    assert_eq!(alt.height, Some(720));
    assert!(!alt.is_default);
}

#[test]
fn test_client_rejects_cross_origin_stream_urls() {
    let client = AdultHubClient::new("http://localhost:8090").expect("client init");

    // Cross-origin absolute stream URL must be rejected
    let hostile = "https://malicious-cdn.com/api/v1/playback/stream/stolen";
    let res = client.resolve_stream_url(hostile);
    match res {
        Err(AdultHubError::OriginMismatch { expected, actual }) => {
            assert!(expected.contains("8090"));
            assert!(actual.contains("malicious-cdn.com"));
        }
        _ => panic!("Expected OriginMismatch error on cross-origin stream URL"),
    }

    // Same origin relative stream path resolves safely
    let rel_path = "/api/v1/playback/stream/telegram:vid_1";
    let resolved = client
        .resolve_stream_url(rel_path)
        .expect("resolve stream path");
    assert_eq!(
        resolved,
        "http://localhost:8090/api/v1/playback/stream/telegram:vid_1"
    );

    // Same origin absolute stream path resolves safely
    let same_origin = "http://localhost:8090/api/v1/playback/stream/telegram:vid_1";
    let resolved2 = client
        .resolve_stream_url(same_origin)
        .expect("resolve stream URL");
    assert_eq!(
        resolved2,
        "http://localhost:8090/api/v1/playback/stream/telegram:vid_1"
    );
}

#[test]
fn test_wire_get_playback_capabilities() {
    let (server_url, handle) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("GET /api/v1/playback/capabilities HTTP/1.1"));
        assert!(headers.contains("authorization: Bearer ah_tok_stream_secret"));
        let body = r#"{
                "streaming_protocols": ["http_range_rfc7233"],
                "supported_containers": ["video/mp4", "video/webm", "video/x-matroska"],
                "seek_alignment_bytes": 1048576,
                "pinned_representation_required": true,
                "supports_head_requests": true
            }"#;
        (
            200,
            vec![("Content-Type", "application/json")],
            body.to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url)
        .expect("init client")
        .with_token("ah_tok_stream_secret");

    let caps = client
        .get_playback_capabilities()
        .expect("get capabilities");
    handle.join().unwrap();

    assert_eq!(caps.streaming_protocols, vec!["http_range_rfc7233"]);
    assert!(caps.pinned_representation_required);
    assert!(caps.supports_head_requests);
}

#[test]
fn test_wire_get_playback_descriptor() {
    let (server_url, handle) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("GET /api/v1/playback/renditions/telegram%3Avid_42 HTTP/1.1"));
        assert!(headers.contains("authorization: Bearer ah_tok_stream_secret"));
        let body = r#"{
                "video_id": "telegram:vid_42",
                "stream_path": "/api/v1/playback/stream/telegram:vid_42",
                "mime_type": "video/mp4",
                "duration_seconds": 1800.0,
                "file_size_bytes": 500000000,
                "width": 1920,
                "height": 1080,
                "renditions": [
                    {
                        "id": "original",
                        "label": "Original Direct Play (1080p)",
                        "mime_type": "video/mp4",
                        "file_size_bytes": 500000000,
                        "width": 1920,
                        "height": 1080,
                        "is_default": true,
                        "stream_path": "/api/v1/playback/stream/telegram:vid_42"
                    }
                ]
            }"#;
        (
            200,
            vec![("Content-Type", "application/json")],
            body.to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url)
        .expect("init client")
        .with_token("ah_tok_stream_secret");

    let desc = client
        .get_playback_descriptor("telegram:vid_42")
        .expect("get descriptor");
    handle.join().unwrap();

    assert_eq!(desc.video_id, "telegram:vid_42");
    assert_eq!(
        desc.default_stream_path(),
        Some("/api/v1/playback/stream/telegram:vid_42")
    );
    assert_eq!(desc.renditions.len(), 1);
    assert!(desc.renditions[0].is_default);

    let full_stream_url = client
        .resolve_stream_url(desc.default_stream_path().unwrap())
        .expect("resolve stream");
    assert_eq!(
        full_stream_url,
        format!("{server_url}/api/v1/playback/stream/telegram:vid_42")
    );
}

#[test]
fn test_wire_build_stream_request_and_head() {
    let client = AdultHubClient::new("http://localhost:8090")
        .expect("client init")
        .with_token("ah_tok_test_auth");

    assert_eq!(
        client.auth_header_value().as_deref(),
        Some("Bearer ah_tok_test_auth")
    );

    // GET request with range
    let req_builder = client
        .build_stream_request(
            "/api/v1/playback/stream/telegram:vid_1",
            Some("bytes=0-1048575"),
        )
        .expect("build GET stream request");
    let req = req_builder.build().expect("build request");
    assert_eq!(req.method(), reqwest::Method::GET);
    assert_eq!(
        req.url().as_str(),
        "http://localhost:8090/api/v1/playback/stream/telegram:vid_1"
    );
    assert_eq!(
        req.headers()
            .get("authorization")
            .and_then(|h| h.to_str().ok()),
        Some("Bearer ah_tok_test_auth")
    );
    assert_eq!(
        req.headers().get("range").and_then(|h| h.to_str().ok()),
        Some("bytes=0-1048575")
    );

    // HEAD request without range
    let head_builder = client
        .build_head_stream_request("/api/v1/playback/stream/telegram:vid_1", None)
        .expect("build HEAD stream request");
    let head_req = head_builder.build().expect("build head request");
    assert_eq!(head_req.method(), reqwest::Method::HEAD);
    assert_eq!(head_req.headers().get("range"), None);
    assert_eq!(
        head_req
            .headers()
            .get("authorization")
            .and_then(|h| h.to_str().ok()),
        Some("Bearer ah_tok_test_auth")
    );

    // Cross origin rejected
    let hostile = client.build_stream_request("http://attacker.com/stream", None);
    assert!(matches!(hostile, Err(AdultHubError::OriginMismatch { .. })));
}

#[test]
fn test_reject_empty_or_whitespace_stream_path_and_prevent_bearer_request() {
    let client = AdultHubClient::new("http://localhost:8090")
        .expect("client init")
        .with_token("bearer_secret_123");

    // resolve_stream_url rejects empty and whitespace paths
    assert!(matches!(
        client.resolve_stream_url(""),
        Err(AdultHubError::InvalidUrl(_))
    ));
    assert!(matches!(
        client.resolve_stream_url("   "),
        Err(AdultHubError::InvalidUrl(_))
    ));
    assert!(matches!(
        client.resolve_stream_url("\t\n"),
        Err(AdultHubError::InvalidUrl(_))
    ));

    // build_stream_request refuses to build request for empty or whitespace paths
    let stream_empty = client.build_stream_request("", None);
    assert!(matches!(stream_empty, Err(AdultHubError::InvalidUrl(_))));

    let stream_ws = client.build_stream_request("   ", Some("bytes=0-100"));
    assert!(matches!(stream_ws, Err(AdultHubError::InvalidUrl(_))));

    // build_head_stream_request refuses to build request for empty or whitespace paths
    let head_empty = client.build_head_stream_request("", None);
    assert!(matches!(head_empty, Err(AdultHubError::InvalidUrl(_))));

    let head_ws = client.build_head_stream_request("  \t ", None);
    assert!(matches!(head_ws, Err(AdultHubError::InvalidUrl(_))));

    // PlaybackDescriptorDto: empty rendition paths filtered when selecting default
    let desc = PlaybackDescriptorDto {
        video_id: "vid_test".into(),
        stream_path: Some("   ".into()), // whitespace top-level path ignored
        mime_type: Some("video/mp4".into()),
        duration_seconds: Some(120.0),
        file_size_bytes: Some(1024),
        width: Some(1920),
        height: Some(1080),
        renditions: vec![
            PlaybackRenditionDto {
                id: "empty_default".into(),
                label: "Empty Default".into(),
                mime_type: Some("video/mp4".into()),
                file_size_bytes: None,
                width: None,
                height: None,
                is_default: true,
                stream_path: Some("".into()), // empty default stream_path
            },
            PlaybackRenditionDto {
                id: "ws_default".into(),
                label: "Whitespace Default".into(),
                mime_type: Some("video/mp4".into()),
                file_size_bytes: None,
                width: None,
                height: None,
                is_default: false,
                stream_path: Some("  \t ".into()), // whitespace stream_path
            },
            PlaybackRenditionDto {
                id: "valid_alt".into(),
                label: "Valid Alt (720p)".into(),
                mime_type: Some("video/mp4".into()),
                file_size_bytes: Some(512),
                width: Some(1280),
                height: Some(720),
                is_default: false,
                stream_path: Some("/api/v1/playback/stream/vid_test?rendition_id=valid_alt".into()),
            },
        ],
    };

    // Fallback correctly finds the first non-empty rendition path
    assert_eq!(
        desc.default_stream_path(),
        Some("/api/v1/playback/stream/vid_test?rendition_id=valid_alt")
    );

    // When all paths are empty, default_stream_path returns None
    let all_empty = PlaybackDescriptorDto {
        video_id: "vid_empty".into(),
        stream_path: None,
        mime_type: None,
        duration_seconds: None,
        file_size_bytes: None,
        width: None,
        height: None,
        renditions: vec![PlaybackRenditionDto {
            id: "r1".into(),
            label: "R1".into(),
            mime_type: None,
            file_size_bytes: None,
            width: None,
            height: None,
            is_default: true,
            stream_path: Some("".into()),
        }],
    };
    assert_eq!(all_empty.default_stream_path(), None);
}

#[test]
fn test_long_stream_request_config_without_sleeping() {
    let client = AdultHubClient::new("http://localhost:8090")
        .expect("client init")
        .with_token("stream_tok");

    // Verify clients are distinct
    assert!(!std::ptr::eq(client.api_client(), client.stream_client()));

    // Ordinary stream requests built with build_stream_request have no per-request timeout capping playback
    let get_req = client
        .build_stream_request(
            "/api/v1/playback/stream/telegram:vid_long",
            Some("bytes=0-"),
        )
        .expect("build stream get")
        .build()
        .expect("build get req");
    assert_eq!(get_req.timeout(), None);

    let head_req = client
        .build_head_stream_request("/api/v1/playback/stream/telegram:vid_long", None)
        .expect("build stream head")
        .build()
        .expect("build head req");
    assert_eq!(head_req.timeout(), None);
}

#[test]
fn test_case_insensitive_url_resolution() {
    let client = AdultHubClient::new("http://localhost:8090").expect("client init");

    // Case-insensitive same-origin scheme and host in resolve_stream_url
    let upper_same_origin = "HTTP://localhost:8090/api/v1/playback/stream/vid_1";
    let res = client
        .resolve_stream_url(upper_same_origin)
        .expect("resolve same origin upper");
    assert_eq!(res, "http://localhost:8090/api/v1/playback/stream/vid_1");

    // Case-insensitive same-origin scheme in resolve_artwork_url
    let upper_art = "HTTP://localhost:8090/api/v1/artwork/video/1/poster";
    let art_res = client
        .resolve_artwork_url(upper_art)
        .expect("resolve art upper");
    assert_eq!(
        art_res,
        "http://localhost:8090/api/v1/artwork/video/1/poster"
    );

    // Case-insensitive cross-origin URL is rejected as OriginMismatch (not prepended to base)
    let hostile_upper = "HTTP://EVIL.COM/malicious_stream";
    let hostile_res = client.resolve_stream_url(hostile_upper);
    assert!(matches!(
        hostile_res,
        Err(AdultHubError::OriginMismatch { .. })
    ));

    let hostile_art = "HTTPS://EVIL.COM/malicious_art";
    let hostile_art_res = client.resolve_artwork_url(hostile_art);
    assert!(matches!(
        hostile_art_res,
        Err(AdultHubError::OriginMismatch { .. })
    ));
}

#[test]
fn test_serialization_error_mapping_on_malformed_json() {
    let (server_url, handle) = spawn_mock_server(|_line, _headers, _body| {
        // Returns 200 OK with malformed JSON body
        (
            200,
            vec![("Content-Type", "application/json")],
            "{malformed-json-here".to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url).expect("init client");
    let res = client.discover_version();
    handle.join().unwrap();

    match res {
        Err(AdultHubError::Serialization(e)) => {
            assert!(!e.to_string().is_empty());
        }
        other => panic!("Expected AdultHubError::Serialization, got: {other:?}"),
    }
}

#[test]
fn test_playback_descriptor_urlencoding_special_characters() {
    let (server_url, handle) =
        spawn_mock_server(|line, _headers, _body| {
            assert!(line.starts_with(
                "GET /api/v1/playback/renditions/telegram%3Ach%231%3Avid%2042 HTTP/1.1"
            ));
            let body = r#"{
                "video_id": "telegram:ch#1:vid 42",
                "stream_path": "/api/v1/playback/stream/telegram:ch#1:vid 42",
                "renditions": []
            }"#;
            (
                200,
                vec![("Content-Type", "application/json")],
                body.to_string(),
            )
        });

    let client = AdultHubClient::new(&server_url).expect("init client");
    let desc = client
        .get_playback_descriptor("telegram:ch#1:vid 42")
        .expect("descriptor");
    handle.join().unwrap();

    assert_eq!(desc.video_id, "telegram:ch#1:vid 42");
}

#[test]
fn test_wire_stream_get_with_range_and_auth() {
    let dummy_chunk = "0123456789abcdef"; // 16 bytes
    let (server_url, handle) = spawn_mock_server(move |line, headers, _body| {
        assert!(line.starts_with("GET /api/v1/playback/stream/telegram:vid_206 HTTP/1.1"));
        assert!(headers.contains("authorization: Bearer stream_secret_tok"));
        assert!(headers.contains("range: bytes=0-15"));

        (
            206,
            vec![
                ("Content-Range", "bytes 0-15/1048576"),
                ("Content-Type", "video/mp4"),
            ],
            dummy_chunk.to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url)
        .expect("init client")
        .with_token("stream_secret_tok");

    let res = client
        .build_stream_request(
            "/api/v1/playback/stream/telegram:vid_206",
            Some("bytes=0-15"),
        )
        .expect("build stream request")
        .send()
        .expect("send request");
    handle.join().unwrap();

    assert_eq!(res.status().as_u16(), 206);
    assert_eq!(
        res.headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok()),
        Some("bytes 0-15/1048576")
    );
    assert_eq!(
        res.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("video/mp4")
    );
    let body = res.text().expect("response text");
    assert_eq!(body, "0123456789abcdef");
}

#[test]
fn test_wire_stream_head_with_auth() {
    let (server_url, handle) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("HEAD /api/v1/playback/stream/telegram:vid_head HTTP/1.1"));
        assert!(headers.contains("authorization: Bearer stream_secret_tok"));
        assert!(!headers.contains("range:"));

        (
            200,
            vec![
                ("Accept-Ranges", "bytes"),
                ("Content-Length", "104857600"),
                ("Content-Type", "video/mp4"),
            ],
            String::new(), // HEAD has empty body
        )
    });

    let client = AdultHubClient::new(&server_url)
        .expect("init client")
        .with_token("stream_secret_tok");

    let res = client
        .build_head_stream_request("/api/v1/playback/stream/telegram:vid_head", None)
        .expect("build head request")
        .send()
        .expect("send head request");
    handle.join().unwrap();

    assert_eq!(res.status().as_u16(), 200);
    assert_eq!(
        res.headers()
            .get("accept-ranges")
            .and_then(|v| v.to_str().ok()),
        Some("bytes")
    );
    assert_eq!(
        res.headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok()),
        Some("104857600")
    );
    assert!(res.bytes().expect("read bytes").is_empty());
}

#[test]
fn test_wire_stream_range_416() {
    let (server_url, handle) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("GET /api/v1/playback/stream/telegram:vid_416 HTTP/1.1"));
        assert!(headers.contains("authorization: Bearer stream_secret_tok"));
        assert!(headers.contains("range: bytes=9999999-"));

        (
            416,
            vec![
                ("Content-Range", "bytes */1024"),
                ("Content-Type", "video/mp4"),
            ],
            String::new(),
        )
    });

    let client = AdultHubClient::new(&server_url)
        .expect("init client")
        .with_token("stream_secret_tok");

    let res = client
        .build_stream_request(
            "/api/v1/playback/stream/telegram:vid_416",
            Some("bytes=9999999-"),
        )
        .expect("build stream request")
        .send()
        .expect("send request");
    handle.join().unwrap();

    assert_eq!(res.status().as_u16(), 416);
    assert_eq!(
        res.headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok()),
        Some("bytes */1024")
    );
}

#[test]
fn test_wire_redirect_302_307_308_never_forward_bearer_cross_origin() {
    for redirect_code in [302, 307, 308] {
        let (server_url, handle) = spawn_mock_server(move |_line, headers, _body| {
            assert!(headers.contains("authorization: Bearer secret_redirect_tok"));
            (
                redirect_code,
                vec![("Location", "https://evil-cross-origin.com/steal_token")],
                "Redirecting".to_string(),
            )
        });

        let client = AdultHubClient::new(&server_url)
            .expect("init client")
            .with_token("secret_redirect_tok");

        // Streaming request with redirect policy none: returns redirect response directly,
        // strictly preventing reqwest from following cross-origin or forwarding bearer token.
        let stream_res = client
            .build_stream_request("/api/v1/playback/stream/vid_red", None)
            .expect("build stream")
            .send()
            .expect("send request");
        handle.join().unwrap();

        assert_eq!(stream_res.status().as_u16(), redirect_code);
        assert_eq!(
            stream_res
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok()),
            Some("https://evil-cross-origin.com/steal_token")
        );
    }
}

#[test]
fn test_wire_error_mapping_401_403_502() {
    // 401 Unauthorized with RFC 7807 problem details
    let (server_url1, handle1) = spawn_mock_server(|_line, _headers, _body| {
        (
                401,
                vec![("Content-Type", "application/problem+json")],
                r#"{"title": "Unauthorized", "detail": "Session token has expired", "status": 401, "code": "TOKEN_EXPIRED"}"#.to_string(),
            )
    });
    let client1 = AdultHubClient::new(&server_url1).expect("init client");
    let err1 = client1.get_playback_capabilities().unwrap_err();
    handle1.join().unwrap();
    assert!(err1.is_unauthorized());
    match err1 {
        AdultHubError::Unauthorized { status, message } => {
            assert_eq!(status, 401);
            assert!(message.contains("Session token has expired"));
        }
        other => panic!("Expected Unauthorized, got: {other:?}"),
    }

    // 403 Forbidden with RFC 7807 problem details
    let (server_url2, handle2) = spawn_mock_server(|_line, _headers, _body| {
        (
                403,
                vec![("Content-Type", "application/problem+json")],
                r#"{"title": "Forbidden", "detail": "Account is disabled", "status": 403, "code": "ACCOUNT_DISABLED"}"#.to_string(),
            )
    });
    let client2 = AdultHubClient::new(&server_url2).expect("init client");
    let err2 = client2.get_playback_capabilities().unwrap_err();
    handle2.join().unwrap();
    assert!(err2.is_unauthorized());
    match err2 {
        AdultHubError::Unauthorized { status, message } => {
            assert_eq!(status, 403);
            assert!(message.contains("Account is disabled"));
        }
        other => panic!("Expected Unauthorized, got: {other:?}"),
    }

    // 502 Bad Gateway with RFC 7807 problem details and code: SOURCE_UNAVAILABLE
    let (server_url3, handle3) = spawn_mock_server(|_line, _headers, _body| {
        (
                502,
                vec![("Content-Type", "application/problem+json")],
                r#"{"title": "Source Unavailable", "detail": "All upstream candidates failed", "status": 502, "code": "SOURCE_UNAVAILABLE"}"#.to_string(),
            )
    });
    let client3 = AdultHubClient::new(&server_url3).expect("init client");
    let err3 = client3.get_playback_capabilities().unwrap_err();
    handle3.join().unwrap();
    match err3 {
        AdultHubError::Api {
            status,
            message,
            code,
        } => {
            assert_eq!(status, 502);
            assert!(message.contains("All upstream candidates failed"));
            assert_eq!(code.as_deref(), Some("SOURCE_UNAVAILABLE"));
        }
        other => panic!("Expected Api 502, got: {other:?}"),
    }
}

#[test]
fn test_catalog_video_item_to_tv_item_mapping() {
    let video_item = CatalogVideoItem {
        id: "telegram:chan_1:msg_42:uid_abc".to_string(),
        title: "Canonical Video Title".to_string(),
        duration_seconds: 120,
        release_date: Some("2026-11-20".to_string()),
        date_added: Some("2026-11-21T10:00:00Z".to_string()),
        studio: Some("Studio Prime".to_string()),
        plot: Some("A canonical test plot summary.".to_string()),
        categories: vec!["HD".to_string(), "Action".to_string()],
        tags: vec!["tag1".to_string()],
        performers: vec![PerformerRefDto {
            id: "perf_1".to_string(),
            name: "Actor One".to_string(),
            photo_url: Some("/api/v1/artwork/actor1.jpg".to_string()),
        }],
        poster_url: Some("/api/v1/artwork/poster.jpg".to_string()),
        backdrop_url: Some("/api/v1/artwork/backdrop.jpg".to_string()),
        file_size_bytes: Some(104857600),
        width: Some(1920),
        height: Some(1080),
        is_hd: Some(true),
        user_state: Some(CatalogVideoUserStateDto {
            is_favorite: Some(true),
            position_seconds: Some(60),
            played_percentage: Some(50.0),
            is_played: Some(false),
            last_watched_at: None,
        }),
    };

    let tv_item = TvItem::from(video_item);
    assert_eq!(tv_item.id, "telegram:chan_1:msg_42:uid_abc");
    assert_eq!(tv_item.title, "Canonical Video Title");
    assert_eq!(tv_item.source_provider.as_deref(), Some("telegram"));
    assert_eq!(
        tv_item.provider_video_id.as_deref(),
        Some("telegram:chan_1:msg_42:uid_abc")
    );
    assert_eq!(
        tv_item.overview.as_deref(),
        Some("A canonical test plot summary.")
    );
    assert_eq!(tv_item.year, Some(2026));
    assert_eq!(tv_item.runtime_ticks, Some(120 * 10_000_000));
    assert_eq!(tv_item.genres, vec!["HD", "Action"]);
    assert_eq!(tv_item.tags, vec!["tag1"]);
    assert_eq!(tv_item.studios, vec!["Studio Prime"]);
    assert_eq!(tv_item.people.len(), 1);
    assert_eq!(tv_item.people[0].name, "Actor One");
    assert_eq!(
        tv_item.people[0].primary_image_tag.as_deref(),
        Some("/api/v1/artwork/actor1.jpg")
    );
    assert_eq!(
        tv_item.primary_image_tag.as_deref(),
        Some("/api/v1/artwork/poster.jpg")
    );
    assert_eq!(
        tv_item.backdrop_image_tag.as_deref(),
        Some("/api/v1/artwork/backdrop.jpg")
    );
    assert!(tv_item.is_favorite);
    assert!(!tv_item.is_played);
    assert_eq!(tv_item.playback_position_ticks, 60 * 10_000_000);
    assert_eq!(tv_item.played_percentage, Some(50.0));
    assert_eq!(tv_item.file_size_bytes, Some(104857600));
    assert_eq!(tv_item.width, Some(1920));
    assert_eq!(tv_item.height, Some(1080));
}

#[test]
fn test_login_error_paths_unauthorized_and_invalid_url() {
    // Invalid URL policy rejection
    let invalid = AdultHubClient::new("ftp://invalid.host");
    assert!(matches!(invalid, Err(AdultHubError::InvalidUrl(_))));

    // 401 Unauthorized password login error path
    let (server_url, handle) = spawn_mock_server(|line, _headers, body| {
        assert!(line.starts_with("POST /api/v1/auth/login "));
        assert!(body.contains("\"username\":\"owner\""));
        (
            401,
            vec![("Content-Type", "application/problem+json")],
            r#"{"title": "Unauthorized", "detail": "Invalid username or password", "status": 401}"#
                .to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url).expect("init client");
    let secret_password = "wrong_pass_plaintext_should_not_leak_in_err";
    let err = client.login("owner", secret_password).unwrap_err();
    handle.join().unwrap();

    assert!(err.is_unauthorized());
    match err {
        AdultHubError::Unauthorized { status, message } => {
            assert_eq!(status, 401);
            assert!(message.contains("Invalid username or password"));
            assert!(!message.contains(secret_password));
        }
        other => panic!("Expected Unauthorized, got: {other:?}"),
    }
}

#[test]
fn test_catalog_videos_sort_query_construction_and_supported_values() {
    let (server_url, handle) = spawn_mock_server(|line, _headers, _body| {
        assert!(line.starts_with("GET /api/v1/catalog/videos?offset=0&limit=16&sort=latest "));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{"items": [], "total_count": 0, "offset": 0, "limit": 16, "has_more": false}"#
                .to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url).expect("init client");
    let query = CatalogVideosQuery {
        offset: Some(0),
        limit: Some(16),
        sort: Some("latest"),
        ..Default::default()
    };
    let resp = client.get_videos(&query).expect("query videos");
    handle.join().unwrap();
    assert_eq!(resp.total_count, 0);
    assert_eq!(resp.items.len(), 0);
}

#[test]
fn test_catalog_videos_direction_query_construction() {
    // Test "asc"
    let (server_url_asc, handle_asc) = spawn_mock_server(|line, _headers, _body| {
        assert!(line.starts_with(
            "GET /api/v1/catalog/videos?offset=0&limit=16&sort=latest&direction=asc "
        ));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{"items": [], "total_count": 0, "offset": 0, "limit": 16, "has_more": false}"#
                .to_string(),
        )
    });
    let client_asc = AdultHubClient::new(&server_url_asc).expect("init client");
    let query_asc = CatalogVideosQuery {
        offset: Some(0),
        limit: Some(16),
        sort: Some("latest"),
        direction: Some("asc"),
        ..Default::default()
    };
    let resp_asc = client_asc.get_videos(&query_asc).expect("query videos");
    handle_asc.join().unwrap();
    assert_eq!(resp_asc.total_count, 0);

    // Test "desc"
    let (server_url_desc, handle_desc) = spawn_mock_server(|line, _headers, _body| {
        assert!(line.starts_with(
            "GET /api/v1/catalog/videos?offset=0&limit=16&sort=title&direction=desc "
        ));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{"items": [], "total_count": 0, "offset": 0, "limit": 16, "has_more": false}"#
                .to_string(),
        )
    });
    let client_desc = AdultHubClient::new(&server_url_desc).expect("init client");
    let query_desc = CatalogVideosQuery {
        offset: Some(0),
        limit: Some(16),
        sort: Some("title"),
        direction: Some("desc"),
        ..Default::default()
    };
    let resp_desc = client_desc.get_videos(&query_desc).expect("query videos");
    handle_desc.join().unwrap();
    assert_eq!(resp_desc.total_count, 0);

    // Test omitted direction preserves query without direction parameter
    let (server_url_none, handle_none) = spawn_mock_server(|line, _headers, _body| {
        assert!(line.starts_with("GET /api/v1/catalog/videos?offset=0&limit=16&sort=latest "));
        assert!(!line.contains("direction="));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{"items": [], "total_count": 0, "offset": 0, "limit": 16, "has_more": false}"#
                .to_string(),
        )
    });
    let client_none = AdultHubClient::new(&server_url_none).expect("init client");
    let query_none = CatalogVideosQuery {
        offset: Some(0),
        limit: Some(16),
        sort: Some("latest"),
        direction: None,
        ..Default::default()
    };
    let resp_none = client_none.get_videos(&query_none).expect("query videos");
    handle_none.join().unwrap();
    assert_eq!(resp_none.total_count, 0);
}

#[test]
fn test_catalog_videos_401_unauthorized_detection() {
    let (server_url, handle) = spawn_mock_server(|line, _headers, _body| {
        assert!(line.starts_with("GET /api/v1/catalog/videos"));
        (
            401,
            vec![("Content-Type", "application/problem+json")],
            r#"{"title": "Unauthorized", "detail": "Session token expired", "status": 401}"#
                .to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url).expect("init client");
    let query = CatalogVideosQuery::default();
    let err = client.get_catalog_videos(&query).unwrap_err();
    handle.join().unwrap();

    assert!(err.is_unauthorized());
    match err {
        AdultHubError::Unauthorized { status, message } => {
            assert_eq!(status, 401);
            assert!(message.contains("Session token expired"));
        }
        other => panic!("Expected Unauthorized, got {other:?}"),
    }
}

#[test]
fn test_watch_progress_get_and_put_endpoints() {
    // 1. Test GET 200 OK
    let (server_url, handle) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("GET /api/v1/user/watch/telegram%3Avid-100"));
        assert!(headers.contains("authorization: Bearer test_token"));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{
                    "video_id": "telegram:vid-100",
                    "position_seconds": 450,
                    "duration_seconds": 1800,
                    "played_percentage": 25.0,
                    "is_played": false,
                    "last_watched_at": "2026-11-22T19:00:00Z"
                }"#
            .to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url)
        .expect("init client")
        .with_token("test_token");
    let progress = client
        .get_watch_progress("telegram:vid-100")
        .expect("get progress")
        .expect("should be some");
    handle.join().unwrap();

    assert_eq!(progress.video_id, "telegram:vid-100");
    assert_eq!(progress.position_seconds, 450);
    assert_eq!(progress.duration_seconds, 1800);
    assert_eq!(progress.played_percentage, 25.0);
    assert!(!progress.is_played);

    // 2. Test GET 404 Not Found returns Ok(None)
    let (server_url_404, handle_404) = spawn_mock_server(|line, _headers, _body| {
        assert!(line.starts_with("GET /api/v1/user/watch/telegram%3Avid-notfound"));
        (
            404,
            vec![("Content-Type", "application/problem+json")],
            r#"{"title": "Not Found", "status": 404}"#.to_string(),
        )
    });

    let client_404 = AdultHubClient::new(&server_url_404)
        .expect("init client")
        .with_token("test_token");
    let progress_none = client_404
        .get_watch_progress("telegram:vid-notfound")
        .expect("get progress 404");
    handle_404.join().unwrap();
    assert!(progress_none.is_none());

    // 3. Test PUT 200 OK
    let (server_url_put, handle_put) = spawn_mock_server(|line, headers, body| {
        assert!(line.starts_with("PUT /api/v1/user/watch/telegram%3Avid-100"));
        assert!(headers.contains("authorization: Bearer test_token"));
        assert!(body.contains(r#""position_seconds":500"#));
        assert!(body.contains(r#""duration_seconds":1800"#));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{"ok": true}"#.to_string(),
        )
    });

    let client_put = AdultHubClient::new(&server_url_put)
        .expect("init client")
        .with_token("test_token");
    let put_req = WatchProgressUpsertRequest {
        position_seconds: 500,
        duration_seconds: 1800,
        is_played: false,
        played_percentage: Some(27.78),
        last_watched_at: None,
    };
    let put_res = client_put
        .put_watch_progress("telegram:vid-100", &put_req)
        .expect("put progress");
    handle_put.join().unwrap();
    assert!(put_res.ok);

    // 4. Test DELETE 200 OK
    let (server_url_del, handle_del) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("DELETE /api/v1/user/watch/telegram%3Avid-100"));
        assert!(headers.contains("authorization: Bearer test_token"));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{"deleted": true}"#.to_string(),
        )
    });

    let client_del = AdultHubClient::new(&server_url_del)
        .expect("init client")
        .with_token("test_token");
    let del_res = client_del
        .delete_watch_progress("telegram:vid-100")
        .expect("delete progress");
    handle_del.join().unwrap();
    assert!(del_res);
}

#[test]
fn test_favorites_wire_auth_crud_and_user_state_mapping() {
    // 1. GET /api/v1/user/favorites/{video_id}
    let (server_url_status, handle_status) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("GET /api/v1/user/favorites/telegram%3Avid-fav-1"));
        assert!(headers.contains("authorization: Bearer token_fav_123"));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{"video_id": "telegram:vid-fav-1", "is_favorite": true}"#.to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url_status)
        .expect("init client")
        .with_token("token_fav_123");
    let status_res = client
        .get_favorite_status("telegram:vid-fav-1")
        .expect("get fav status");
    handle_status.join().unwrap();
    assert_eq!(status_res.video_id.as_deref(), Some("telegram:vid-fav-1"));
    assert!(status_res.is_favorite);

    // 2. PUT /api/v1/user/favorites/{video_id}
    let (server_url_add, handle_add) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("PUT /api/v1/user/favorites/telegram%3Avid-fav-1"));
        assert!(headers.contains("authorization: Bearer token_fav_123"));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{"video_id": "telegram:vid-fav-1", "is_favorite": true}"#.to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url_add)
        .expect("init client")
        .with_token("token_fav_123");
    let add_res = client.add_favorite("telegram:vid-fav-1").expect("add fav");
    handle_add.join().unwrap();
    assert!(add_res.is_favorite);

    // 3. DELETE /api/v1/user/favorites/{video_id}
    let (server_url_rem, handle_rem) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("DELETE /api/v1/user/favorites/telegram%3Avid-fav-1"));
        assert!(headers.contains("authorization: Bearer token_fav_123"));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{"video_id": "telegram:vid-fav-1", "is_favorite": false}"#.to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url_rem)
        .expect("init client")
        .with_token("token_fav_123");
    let rem_res = client
        .remove_favorite("telegram:vid-fav-1")
        .expect("remove fav");
    handle_rem.join().unwrap();
    assert!(!rem_res.is_favorite);

    // 4. GET /api/v1/user/favorites?offset=10&limit=25
    let (server_url_list, handle_list) = spawn_mock_server(|line, headers, _body| {
        assert!(line.starts_with("GET /api/v1/user/favorites?offset=10&limit=25"));
        assert!(headers.contains("authorization: Bearer token_fav_123"));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{
                    "items": [
                        {
                            "id": "telegram:fav-vid-canonical",
                            "title": "Favorite Scene 1",
                            "duration_seconds": 1200,
                            "user_state": {
                                "is_favorite": true,
                                "position_seconds": 300,
                                "played_percentage": 25.0,
                                "is_played": false
                            }
                        }
                    ],
                    "total_count": 1,
                    "offset": 10,
                    "limit": 25,
                    "has_more": false
                }"#
            .to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url_list)
        .expect("init client")
        .with_token("token_fav_123");
    let list_res = client
        .get_favorites(Some(10), Some(25), None, None)
        .expect("get favorites list");
    handle_list.join().unwrap();
    assert_eq!(list_res.total_count, 1);
    assert_eq!(list_res.items.len(), 1);
    assert!(!list_res.has_more());

    // 5. User state mapping to TvItem
    let cat_item = list_res.items.into_iter().next().unwrap();
    let tv_item = TvItem::from(cat_item);
    assert_eq!(tv_item.id, "telegram:fav-vid-canonical");
    assert!(tv_item.is_favorite);
    assert_eq!(tv_item.playback_position_ticks, 3_000_000_000);
    assert!(!tv_item.is_played);

    // Test user_state is_favorite=false mapping
    let non_fav_item = CatalogVideoItem {
        id: "vid-2".into(),
        title: "Non Fav".into(),
        duration_seconds: 100,
        release_date: None,
        date_added: None,
        studio: None,
        plot: None,
        categories: vec![],
        tags: vec![],
        performers: vec![],
        poster_url: None,
        backdrop_url: None,
        file_size_bytes: None,
        width: None,
        height: None,
        is_hd: None,
        user_state: Some(CatalogVideoUserStateDto {
            is_favorite: Some(false),
            position_seconds: None,
            played_percentage: None,
            is_played: None,
            last_watched_at: None,
        }),
    };
    let tv_non_fav = TvItem::from(non_fav_item);
    assert!(!tv_non_fav.is_favorite);

    // 6. Test CatalogVideosQuery with is_favorite
    let (server_url_query, handle_query) = spawn_mock_server(|line, headers, _body| {
        assert!(line.contains("is_favorite=true"));
        assert!(headers.contains("authorization: Bearer token_fav_123"));
        (
            200,
            vec![("Content-Type", "application/json")],
            r#"{"items": [], "total_count": 0, "offset": 0, "limit": 50, "has_more": false}"#
                .to_string(),
        )
    });

    let client = AdultHubClient::new(&server_url_query)
        .expect("init client")
        .with_token("token_fav_123");
    let q = CatalogVideosQuery {
        is_favorite: Some(true),
        ..Default::default()
    };
    let _ = client.get_videos(&q).expect("query with is_favorite");
    handle_query.join().unwrap();
}

#[test]
fn test_favorites_wire_query_sort_and_direction() {
    let (server_url, handle) =
        spawn_mock_server(|line, headers, _body| {
            assert!(line.starts_with(
                "GET /api/v1/user/favorites?offset=0&limit=50&sort=latest&direction=asc"
            ));
            assert!(headers.contains("authorization: Bearer tok_sort_test"));
            (
                200,
                vec![("Content-Type", "application/json")],
                r#"{
                    "items": [],
                    "total_count": 0,
                    "offset": 0,
                    "limit": 50,
                    "has_more": false
                }"#
                .to_string(),
            )
        });

    let client = AdultHubClient::new(&server_url)
        .expect("init client")
        .with_token("tok_sort_test");
    let res = client
        .get_favorites(Some(0), Some(50), Some("latest"), Some("asc"))
        .expect("get favorites with sort and direction");
    handle.join().unwrap();
    assert_eq!(res.total_count, 0);
    assert!(!res.has_more());
}

#[test]
fn test_in_flight_guard_concurrent_deduplication() {
    let guard = InFlightGuard::new();
    let key = "telegram:-100123:456:789";

    assert!(!guard.is_in_flight(key));

    // First click acquires slot
    let slot1 = guard.acquire(key);
    assert!(slot1.is_some());
    assert!(guard.is_in_flight(key));

    // Rapid second click for same item fails to acquire
    let slot2 = guard.acquire(key);
    assert!(slot2.is_none());
    assert!(guard.is_in_flight(key));

    // Different item can still acquire independently
    let other_slot = guard.acquire("telegram:-100999:111:222");
    assert!(other_slot.is_some());
    assert!(guard.is_in_flight("telegram:-100999:111:222"));

    // When first slot is dropped (async operation completes), slot is freed
    drop(slot1);
    assert!(!guard.is_in_flight(key));

    // Subsequent click for first item can now acquire successfully
    let slot3 = guard.acquire(key);
    assert!(slot3.is_some());
    assert!(guard.is_in_flight(key));
}

#[test]
fn test_playback_descriptor_rendition_stream_path_resolution() {
    let desc = PlaybackDescriptorDto {
        video_id: "telegram:-100123:456:789".into(),
        stream_path: Some("/api/v1/playback/stream/telegram:-100123:456:789".into()),
        mime_type: Some("video/mp4".into()),
        duration_seconds: Some(120.0),
        file_size_bytes: Some(10485760),
        width: Some(1920),
        height: Some(1080),
        renditions: vec![
            PlaybackRenditionDto {
                id: "original".into(),
                label: "Original (1080p)".into(),
                mime_type: Some("video/mp4".into()),
                file_size_bytes: Some(10485760),
                width: Some(1920),
                height: Some(1080),
                is_default: true,
                stream_path: Some("/api/v1/playback/stream/telegram:-100123:456:789".into()),
            },
            PlaybackRenditionDto {
                id: "alt_720".into(),
                label: "720p".into(),
                mime_type: Some("video/mp4".into()),
                file_size_bytes: Some(5242880),
                width: Some(1280),
                height: Some(720),
                is_default: false,
                stream_path: Some(
                    "/api/v1/playback/stream/telegram:-100123:456:789?rendition_id=alt_720".into(),
                ),
            },
            PlaybackRenditionDto {
                id: "broken".into(),
                label: "Broken".into(),
                mime_type: Some("video/mp4".into()),
                file_size_bytes: None,
                width: None,
                height: None,
                is_default: false,
                stream_path: Some("   ".into()),
            },
        ],
    };

    // None or "original" resolves default stream path
    assert_eq!(
        desc.rendition_stream_path(None),
        Some("/api/v1/playback/stream/telegram:-100123:456:789")
    );
    assert_eq!(
        desc.rendition_stream_path(Some("original")),
        Some("/api/v1/playback/stream/telegram:-100123:456:789")
    );

    // Requested existing rendition resolves its stream path
    assert_eq!(
        desc.rendition_stream_path(Some("alt_720")),
        Some("/api/v1/playback/stream/telegram:-100123:456:789?rendition_id=alt_720")
    );

    // Non-existent rendition safely falls back to default
    assert_eq!(
        desc.rendition_stream_path(Some("nonexistent_480")),
        Some("/api/v1/playback/stream/telegram:-100123:456:789")
    );

    // Rendition with whitespace stream path safely falls back to default
    assert_eq!(
        desc.rendition_stream_path(Some("broken")),
        Some("/api/v1/playback/stream/telegram:-100123:456:789")
    );
}

#[test]
fn test_evaluate_range_resume_matching_206() {
    let outcome = evaluate_range_resume(
        206,
        Some("bytes 1048576-2097151/2097152"),
        Some(1048576),
        1048576,
        Some(2097152),
    );
    assert_eq!(
        outcome,
        RangeResumeOutcome::Resume {
            start_byte: 1048576,
            total_bytes: Some(2097152),
        }
    );

    // Resuming without prior known total succeeds and records server total
    let outcome2 = evaluate_range_resume(206, Some("bytes 500-999/1000"), Some(500), 500, None);
    assert_eq!(
        outcome2,
        RangeResumeOutcome::Resume {
            start_byte: 500,
            total_bytes: Some(1000),
        }
    );
}

#[test]
fn test_evaluate_range_resume_mismatched_start_offset() {
    // Server returned range starting at 0 when client asked from 500
    let outcome = evaluate_range_resume(206, Some("bytes 0-999/1000"), Some(1000), 500, Some(1000));
    assert!(matches!(
        outcome,
        RangeResumeOutcome::Restart { ref reason, total_bytes: Some(1000) }
            if reason.contains("does not match local existing bytes")
    ));

    // Server returned range starting at 200 when client had 500
    let outcome2 =
        evaluate_range_resume(206, Some("bytes 200-999/1000"), Some(800), 500, Some(1000));
    assert!(matches!(
        outcome2,
        RangeResumeOutcome::Restart { ref reason, .. }
            if reason.contains("does not match local existing bytes")
    ));
}

#[test]
fn test_evaluate_range_resume_mismatched_total_file_size() {
    // Server file size changed from 1000 to 2000 (different encoding or media source)
    let outcome = evaluate_range_resume(
        206,
        Some("bytes 500-1999/2000"),
        Some(1500),
        500,
        Some(1000),
    );
    assert!(matches!(
        outcome,
        RangeResumeOutcome::Restart { ref reason, total_bytes: Some(2000) }
            if reason.contains("file size changed from 1000 to 2000")
    ));
}

#[test]
fn test_evaluate_range_resume_http_200_with_existing_bytes() {
    // Server returned full file (HTTP 200) ignoring Range request
    let outcome = evaluate_range_resume(200, None, Some(5000), 1000, Some(5000));
    assert!(matches!(
        outcome,
        RangeResumeOutcome::Restart { ref reason, total_bytes: Some(5000) }
            if reason.contains("ignoring Range request")
    ));

    // Starting from byte 0 with 200 OK is a normal clean download
    let outcome2 = evaluate_range_resume(200, None, Some(5000), 0, Some(5000));
    assert_eq!(
        outcome2,
        RangeResumeOutcome::Resume {
            start_byte: 0,
            total_bytes: Some(5000),
        }
    );
}

#[test]
fn test_evaluate_range_resume_http_416() {
    // Partial bytes on disk out of bounds -> Restart cleanly
    let outcome = evaluate_range_resume(416, Some("bytes */1000"), None, 1200, Some(1000));
    assert!(matches!(
        outcome,
        RangeResumeOutcome::Restart { ref reason, .. }
            if reason.contains("local partial bytes out of bounds")
    ));

    // If existing bytes is 0 and 416 occurs -> Fatal error
    let outcome2 = evaluate_range_resume(416, Some("bytes */0"), None, 0, None);
    assert!(matches!(
        outcome2,
        RangeResumeOutcome::Error { status: 416, .. }
    ));
}

#[test]
fn test_evaluate_range_resume_http_401_and_errors() {
    let outcome_401 = evaluate_range_resume(401, None, None, 500, Some(1000));
    assert!(matches!(
        outcome_401,
        RangeResumeOutcome::Error { status: 401, ref message }
            if message.contains("Session expired")
    ));

    let outcome_502 = evaluate_range_resume(502, None, None, 500, Some(1000));
    assert!(matches!(
        outcome_502,
        RangeResumeOutcome::Error { status: 502, .. }
    ));
}

#[test]
fn test_wire_download_stream_with_bearer_auth_scoping() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let address = listener.local_addr().expect("local addr");
    let server_url = format!("http://{address}");

    let handle = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept");
        let mut req_buf = [0u8; 2048];
        let n = socket.read(&mut req_buf).expect("read");
        let req_str = String::from_utf8_lossy(&req_buf[..n]);

        // Verify requested URL and headers:
        assert!(req_str.starts_with("GET /api/v1/playback/stream/canonical_123 HTTP/1.1"));
        // Bearer token strictly in Authorization header:
        assert!(
            req_str.contains("authorization: Bearer ah_tok_download_test\r\n")
                || req_str.contains("Authorization: Bearer ah_tok_download_test\r\n")
        );
        // Range header present:
        assert!(
            req_str.contains("range: bytes=100-\r\n") || req_str.contains("Range: bytes=100-\r\n")
        );
        // Bearer token NEVER in URL path or query string:
        assert!(
            !req_str
                .lines()
                .next()
                .unwrap()
                .contains("ah_tok_download_test")
        );

        let body = b"streamdata12345";
        let resp = format!(
            "HTTP/1.1 206 Partial Content\r\n\
                 Content-Type: application/octet-stream\r\n\
                 Accept-Ranges: bytes\r\n\
                 Content-Range: bytes 100-114/200\r\n\
                 Content-Length: {}\r\n\
                 Cache-Control: private, no-store\r\n\
                 \r\n",
            body.len()
        );
        socket.write_all(resp.as_bytes()).expect("write");
        socket.write_all(body).expect("write body");
    });

    let client = AdultHubClient::new(&server_url)
        .expect("init client")
        .with_token("ah_tok_download_test");

    let stream_url = client
        .resolve_stream_url("/api/v1/playback/stream/canonical_123")
        .expect("resolve stream URL");
    assert_eq!(
        stream_url,
        format!("{server_url}/api/v1/playback/stream/canonical_123")
    );

    let req_builder = client
        .build_stream_request(&stream_url, Some("bytes=100-"))
        .expect("build request");
    let mut resp = req_builder.send().expect("send request");
    assert_eq!(resp.status().as_u16(), 206);
    let mut buf = Vec::new();
    resp.read_to_end(&mut buf).expect("read body");
    assert_eq!(buf, b"streamdata12345");

    handle.join().unwrap();
}

#[test]
fn test_wire_download_token_renewal_flow() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let address = listener.local_addr().expect("local addr");
    let server_url = format!("http://{address}");

    let handle = std::thread::spawn(move || {
        // Request 1: GET stream with expired token -> returns 401
        {
            let (mut socket, _) = listener.accept().expect("accept 1");
            let mut buf = [0u8; 2048];
            let n = socket.read(&mut buf).expect("read 1");
            let req_str = String::from_utf8_lossy(&buf[..n]);
            assert!(req_str.contains("Bearer ah_tok_expired"));

            let resp = "HTTP/1.1 401 Unauthorized\r\n\
                            Content-Type: application/problem+json\r\n\
                            Cache-Control: private, no-store\r\n\
                            Content-Length: 35\r\n\
                            \r\n\
                            {\"error\":\"Session token is expired\"}";
            socket.write_all(resp.as_bytes()).expect("write 401");
        }

        // Request 2: POST /api/v1/auth/refresh -> returns fresh tokens
        {
            let (mut socket, _) = listener.accept().expect("accept 2");
            let mut buf = [0u8; 2048];
            let n = socket.read(&mut buf).expect("read 2");
            let req_str = String::from_utf8_lossy(&buf[..n]);
            assert!(req_str.starts_with("POST /api/v1/auth/refresh HTTP/1.1"));
            assert!(req_str.contains("\"refresh_token\":\"ah_ref_valid_rotate\""));

            let refresh_body = r#"{"access_token":"ah_tok_renewed_456","token_type":"Bearer","expires_in":900,"refresh_token":"ah_ref_rotated_789"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\n\
                     Content-Type: application/json\r\n\
                     Cache-Control: private, no-store\r\n\
                     Content-Length: {}\r\n\
                     \r\n\
                     {}",
                refresh_body.len(),
                refresh_body
            );
            socket.write_all(resp.as_bytes()).expect("write refresh");
        }

        // Request 3: GET stream retried with renewed token -> returns 206 Partial Content
        {
            let (mut socket, _) = listener.accept().expect("accept 3");
            let mut buf = [0u8; 2048];
            let n = socket.read(&mut buf).expect("read 3");
            let req_str = String::from_utf8_lossy(&buf[..n]);
            assert!(req_str.contains("Bearer ah_tok_renewed_456"));
            assert!(!req_str.contains("ah_tok_expired"));
            assert!(req_str.contains("bytes=256-"));

            let body = b"renewed_stream_payload";
            let resp = format!(
                "HTTP/1.1 206 Partial Content\r\n\
                     Content-Type: application/octet-stream\r\n\
                     Accept-Ranges: bytes\r\n\
                     Content-Range: bytes 256-277/500\r\n\
                     Content-Length: {}\r\n\
                     Cache-Control: private, no-store\r\n\
                     \r\n",
                body.len()
            );
            socket.write_all(resp.as_bytes()).expect("write stream");
            socket.write_all(body).expect("write body");
        }
    });

    let client = AdultHubClient::new(&server_url)
        .expect("init client")
        .with_token("ah_tok_expired");

    // Attempt 1 fails with 401
    let req1 = client
        .build_stream_request("/api/v1/playback/stream/item1", Some("bytes=256-"))
        .unwrap();
    let res1 = req1.send().expect("send initial");
    assert_eq!(res1.status().as_u16(), 401);

    // Client detects 401 and calls refresh_token
    let refreshed = client
        .refresh_token("ah_ref_valid_rotate")
        .expect("refresh token");
    assert_eq!(refreshed.access_token, "ah_tok_renewed_456");
    assert_eq!(refreshed.refresh_token, "ah_ref_rotated_789");
    assert_eq!(client.token().as_deref(), Some("ah_tok_renewed_456"));

    // Attempt 2 succeeds with renewed Bearer token
    let req2 = client
        .build_stream_request("/api/v1/playback/stream/item1", Some("bytes=256-"))
        .unwrap();
    let mut res2 = req2.send().expect("send retried");
    assert_eq!(res2.status().as_u16(), 206);
    let mut data = Vec::new();
    res2.read_to_end(&mut data).expect("read data");
    assert_eq!(data, b"renewed_stream_payload");

    handle.join().unwrap();
}

#[test]
fn test_part_file_metadata_lifecycle_and_matching() {
    let temp_dir = std::env::temp_dir().join(format!("ah-meta-test-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let part_file = temp_dir.join("test_video.mp4.part");

    // Before creation, sidecar does not exist
    assert!(PartFileMetadata::read_from_sidecar(&part_file).is_none());

    let meta = PartFileMetadata::new(
        "vid_42",
        Some("720p".to_string()),
        "adulthub:http://127.0.0.1:8090|vid_42|720p|/stream/720",
        Some(50_000_000),
    );

    // Matching checks
    assert!(meta.matches(
        "vid_42",
        Some("720p"),
        "adulthub:http://127.0.0.1:8090|vid_42|720p|/stream/720"
    ));
    // Rendition mismatch:
    assert!(!meta.matches(
        "vid_42",
        Some("1080p"),
        "adulthub:http://127.0.0.1:8090|vid_42|720p|/stream/720"
    ));
    assert!(!meta.matches(
        "vid_42",
        None,
        "adulthub:http://127.0.0.1:8090|vid_42|720p|/stream/720"
    ));
    // Item ID mismatch:
    assert!(!meta.matches(
        "vid_99",
        Some("720p"),
        "adulthub:http://127.0.0.1:8090|vid_42|720p|/stream/720"
    ));
    // Source identity mismatch:
    assert!(!meta.matches(
        "vid_42",
        Some("720p"),
        "adulthub:http://127.0.0.1:8090|vid_42|720p|/stream/different"
    ));

    // Persistence to sidecar
    meta.save_to_sidecar(&part_file).expect("save sidecar");
    let loaded = PartFileMetadata::read_from_sidecar(&part_file).expect("read sidecar");
    assert_eq!(loaded, meta);

    // Removal
    PartFileMetadata::remove_sidecar(&part_file);
    assert!(PartFileMetadata::read_from_sidecar(&part_file).is_none());

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_part_file_metadata_optional_unknown_expected_bytes() {
    let meta = PartFileMetadata::new(
        "vid_42",
        None,
        "adulthub:http://127.0.0.1:8090|vid_42|default|/stream",
        None,
    );
    let json = serde_json::to_string(&meta).expect("serialize");
    let deserialized: PartFileMetadata = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(deserialized.expected_total_bytes, None);
    assert!(deserialized.matches(
        "vid_42",
        None,
        "adulthub:http://127.0.0.1:8090|vid_42|default|/stream"
    ));
}
