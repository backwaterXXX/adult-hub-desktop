use adult_hub_client::{AdultHubClient, CatalogTaxonomyQuery, TvItem};
use adult_hub_storage::Session;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const HOME_CATEGORIES_TTL: Duration = Duration::from_secs(60);

type HomeCategoriesCache = Mutex<std::collections::HashMap<String, (Instant, Vec<TvItem>)>>;

static HOME_CATEGORIES_CACHE: OnceLock<HomeCategoriesCache> = OnceLock::new();

pub(super) fn clear_home_categories_cache() {
    if let Some(cache) = HOME_CATEGORIES_CACHE.get() {
        cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

pub(super) fn cached_home_categories(
    client: &AdultHubClient,
    session: &Session,
) -> std::result::Result<Vec<TvItem>, String> {
    let cache = HOME_CATEGORIES_CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let cached = {
        let guard = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .get(&session.server_url)
            .filter(|(cached_at, _)| cached_at.elapsed() < HOME_CATEGORIES_TTL)
            .map(|(_, items)| items.clone())
    };
    if let Some(items) = cached {
        return Ok(items);
    }
    let response = client
        .get_categories(&CatalogTaxonomyQuery {
            offset: Some(0),
            limit: Some(200),
            q: None,
        })
        .map_err(|error| error.to_string())?;
    let items = response
        .items
        .into_iter()
        .map(|item| item.to_category_tv_item())
        .collect::<Vec<_>>();
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session.server_url.clone(), (Instant::now(), items.clone()));
    Ok(items)
}

fn looks_like_telegram_ref(value: &str) -> bool {
    let raw = value.trim().trim_start_matches("telegram:");
    let mut parts = raw.split(':');
    let _channel = parts.next();
    let message = parts.next();
    let file = parts.next();
    parts.next().is_none()
        && message.is_some_and(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        && file.is_some_and(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

pub(super) fn resume_title_needs_hydration(item: &TvItem) -> bool {
    let title = item.title.trim();
    title.is_empty()
        || title.eq_ignore_ascii_case(item.id.as_str())
        || title.starts_with("telegram:")
        || title
            .strip_prefix("Telegram ")
            .is_some_and(looks_like_telegram_ref)
        // Older rows can carry the bare provider reference as the title
        // (`channel:message:file`) while the id has the `telegram:` prefix.
        || looks_like_telegram_ref(title)
}

/// True when a row still shows a raw reference after hydration, which means the
/// catalog no longer knows this material. Such rows are hidden from Home and
/// history instead of exposing `channel:message:file`.
pub(super) fn has_unresolved_raw_title(item: &TvItem) -> bool {
    let title = item.title.trim();
    title.eq_ignore_ascii_case(item.id.trim()) || looks_like_telegram_ref(title)
}

/// Catalog ids for Telegram material carry the `telegram:` prefix; local rows
/// sometimes stored only the bare provider reference.
pub(super) fn catalog_lookup_id(id: &str) -> String {
    let trimmed = id.trim();
    if trimmed.starts_with("telegram:") {
        trimmed.to_string()
    } else if looks_like_telegram_ref(trimmed) {
        format!("telegram:{trimmed}")
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_title_hydration_detects_ids_but_keeps_real_titles() {
        let mut item = TvItem::new(
            "telegram:-1001618765010:141:5337054953330449443",
            "Telegram -1001618765010:141:5337054953330449443",
        );
        assert!(resume_title_needs_hydration(&item));
        item.title = "telegram:-1001618765010:141:5337054953330449443".into();
        assert!(resume_title_needs_hydration(&item));
        item.title = "".into();
        assert!(resume_title_needs_hydration(&item));
        item.title = "Screwed 1".into();
        assert!(!resume_title_needs_hydration(&item));
    }

    #[test]
    fn categories_are_cached_per_server() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 2048];
            let _ = stream.read(&mut buffer);
            let body = r#"{"items":[{"name":"Action","count":3}],"total_count":1,"offset":0,"limit":100,"has_more":false}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        let session = Session {
            server_url: format!("http://{address}"),
            user_id: "u".into(),
            username: "tester".into(),
            access_token: "token".into(),
            server_name: "test".into(),
            refresh_token: None,
        };
        let client = AdultHubClient::new(&session.server_url)
            .unwrap()
            .with_token(&session.access_token);
        let first = cached_home_categories(&client, &session).expect("first load");
        let second = cached_home_categories(&client, &session).expect("cached load");
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        let _ = server.join();
        clear_home_categories_cache();
    }

    #[test]
    fn unresolved_raw_titles_are_recognised_for_hiding() {
        let mut item = TvItem::new(
            "telegram:studioarchivez:15408:5868319077583298087",
            "studioarchivez:15408:5868319077583298087",
        );
        assert!(has_unresolved_raw_title(&item));
        item.title = "Real Title".into();
        assert!(!has_unresolved_raw_title(&item));
        let mut bare = TvItem::new("studioarchivez:15408:5868319077583298087", "");
        bare.title = bare.id.clone();
        assert!(has_unresolved_raw_title(&bare));
    }

    #[test]
    fn bare_provider_reference_needs_hydration_and_gets_the_prefix() {
        let mut item = TvItem::new(
            "telegram:mencompremium:11612:5967464115529057997",
            "mencompremium:11612:5967464115529057997",
        );
        assert!(resume_title_needs_hydration(&item));
        assert_eq!(
            catalog_lookup_id(&item.id),
            "telegram:mencompremium:11612:5967464115529057997"
        );
        assert_eq!(
            catalog_lookup_id("mencompremium:11612:5967464115529057997"),
            "telegram:mencompremium:11612:5967464115529057997"
        );
        assert_eq!(
            catalog_lookup_id("864d93c2-630d-5cfc-9022-a20ec78a352e"),
            "864d93c2-630d-5cfc-9022-a20ec78a352e"
        );
        item.title = "Real Title".into();
        assert!(!resume_title_needs_hydration(&item));
    }

    #[test]
    fn telegram_reference_detection_requires_three_numeric_parts() {
        assert!(looks_like_telegram_ref(
            "-1001618765010:141:5337054953330449443"
        ));
        assert!(looks_like_telegram_ref(
            "telegram:-1001618765010:141:5337054953330449443"
        ));
        assert!(!looks_like_telegram_ref(
            "telegram:5c23c503c114b8a273fd6ec9fbe0a031"
        ));
        assert!(!looks_like_telegram_ref("Movie: Part 1"));
    }
}
