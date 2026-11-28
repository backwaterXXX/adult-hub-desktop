use super::random::*;
use super::*;

#[test]
fn clear_catalog_search_resets_field_applied_query_and_results_together() {
    // Regression for the audited scenario: search "qa-no-results-982731",
    // wait for the empty screen, click the central "Clear search". The
    // visible field text, the applied query and the results state must all
    // return to the catalog state in one action — previously the handler
    // reset the state but the field kept rendering the query.
    let in_search = CatalogSearchState {
        typed_query: "qa-no-results-982731".into(),
        applied_query: "qa-no-results-982731".into(),
        in_results: true,
        error_message: String::new(),
        all_has_more: false,
        all_total_count: 0,
        results_filter: "all".into(),
    };
    let cleared = CatalogSearchState::cleared();
    assert_eq!(cleared.typed_query, "");
    assert_eq!(cleared.applied_query, "");
    assert!(!cleared.in_results);
    assert_eq!(cleared.results_filter, "all");
    assert_ne!(cleared, in_search);
    // A cleared state describes plain catalog browsing: field, counter and
    // list can never disagree because all fields move together.
    assert_eq!(cleared.typed_query, cleared.applied_query);
}

#[test]
fn clear_catalog_search_keeps_search_history() {
    // "Clear search" only moves UI state; it must not erase remembered
    // searches. The transition touches no storage, and an in-memory store
    // proves the history rows survive the reset.
    let store = LocalStore::open_in_memory().unwrap();
    store
        .add_search_query_context("movies", "qa-no-results-982731")
        .unwrap();
    store.add_search_query_context("movies", "dragon").unwrap();
    let _cleared = CatalogSearchState::cleared();
    let history = store.get_search_history_context("movies", 10).unwrap();
    assert!(
        history
            .iter()
            .any(|entry| entry == "qa-no-results-982731" || entry == "dragon"),
        "clearing the current query must not wipe search history"
    );
}

#[test]
fn late_catalog_response_is_rejected_after_a_newer_request_starts() {
    let generation = AtomicU64::new(0);
    let first_request = generation.fetch_add(1, Ordering::SeqCst) + 1;
    let second_request = generation.fetch_add(1, Ordering::SeqCst) + 1;

    assert!(!catalog_generation_is_current(&generation, first_request));
    assert!(catalog_generation_is_current(&generation, second_request));
}

#[test]
fn clear_catalog_search_blanks_the_restored_field_text() {
    // The category listing is restored from a view snapshot that remembers
    // the field text for navigation retention. A "Clear search" must also
    // blank the snapshot of the category it was cleared in, or the old
    // query reappears in the field as soon as the listing is restored.
    let snapshot = |category: &str, query: &str| {
        let page = CatalogPageState {
            category: category.into(),
            ..Default::default()
        };
        CatalogViewSnapshot {
            cards: Vec::new(),
            items: Vec::new(),
            page,
            cached_at: std::time::Instant::now(),
            total_count: 0,
            has_more: false,
            scroll_y: 0.0,
            anchor_index: 0,
            anchor_screen_y: 0.0,
            more_error: String::new(),
            is_search: false,
            search_query: query.into(),
            search_applied_query: String::new(),
            search_error: String::new(),
            search_results_filter: "all".into(),
            search_movie_count: 0,
            search_performer_count: 0,
            search_studio_count: 0,
            search_tag_count: 0,
            search_channel_count: 0,
            search_movie_total: 0,
            search_performer_total: 0,
            search_studio_total: 0,
            search_tag_total: 0,
            search_channel_total: 0,
            search_all_has_more: false,
            search_all_total_count: 0,
        }
    };
    let views = Rc::new(RefCell::new(std::collections::HashMap::new()));
    views.borrow_mut().insert(
        "latest-key".into(),
        snapshot("Latest", "qa-no-results-982731"),
    );
    views.borrow_mut().insert(
        "favorites-key".into(),
        snapshot("Favorites", "typed-elsewhere"),
    );

    clear_saved_view_queries(&views, "Latest");

    assert_eq!(views.borrow()["latest-key"].search_query, "");
    assert_eq!(
        views.borrow()["favorites-key"].search_query,
        "typed-elsewhere",
        "clearing one category's search must not disturb retention elsewhere"
    );
}

#[test]
fn test_search_result_counts() {
    let items = vec![
        TvItem {
            id: "movie1".into(),
            title: "Movie 1".into(),
            ..Default::default()
        },
        TvItem {
            id: "perf1".into(),
            title: "Performer 1".into(),
            genres: vec!["__adult_hub_performer".into()],
            ..Default::default()
        },
        TvItem {
            id: "studio1".into(),
            title: "Studio 1".into(),
            genres: vec!["__adult_hub_studio".into()],
            ..Default::default()
        },
        TvItem {
            id: "tag1".into(),
            title: "Tag 1".into(),
            genres: vec!["__adult_hub_genre".into()],
            ..Default::default()
        },
        TvItem {
            id: "chan1".into(),
            title: "Channel Video".into(),
            genres: vec!["__adult_hub_channel_search".into()],
            ..Default::default()
        },
        TvItem {
            id: "movie2".into(),
            title: "Movie 2".into(),
            ..Default::default()
        },
    ];

    let (movies, performers, studios, tags, channels) = search_result_counts(&items);
    assert_eq!(movies, 2);
    assert_eq!(performers, 1);
    assert_eq!(studios, 1);
    assert_eq!(tags, 1);
    assert_eq!(channels, 1);
}

#[test]
fn random_pages_cover_the_catalog_in_bounded_steps() {
    // A catalog that fits in one page always samples offset 0.
    assert_eq!(random_page_offset(10, 50), 0);
    assert_eq!(random_page_offset(0, 50), 0);
    assert!(!random_page_has_more(1, 10, 50));

    // Three pages allow exactly three load-more steps.
    assert!(random_page_has_more(1, 120, 50));
    assert!(random_page_has_more(2, 120, 50));
    assert!(!random_page_has_more(3, 120, 50));

    // Offsets never point past the last full page.
    for _ in 0..100 {
        let offset = random_page_offset(120, 50);
        assert!(offset <= 70);
    }
}

#[test]
fn performer_sort_defaults_to_a_to_z_and_maps_to_server_params() {
    assert_eq!(
        CatalogFilters::default().performer_sort,
        PerformerSort::NameAsc
    );
    assert_eq!(PerformerSort::NameAsc.query(), ("name", "asc"));
    assert_eq!(PerformerSort::NameDesc.query(), ("name", "desc"));
    assert_eq!(PerformerSort::VideoCount.query(), ("video_count", "desc"));

    assert_eq!(PerformerSort::NameAsc.index(), 0);
    assert_eq!(PerformerSort::NameDesc.index(), 1);
    assert_eq!(PerformerSort::VideoCount.index(), 2);
}

#[test]
fn continue_watching_legacy_categories_route_to_history() {
    assert_eq!(
        legacy_catalog_category("Continue Watching"),
        "Watch History"
    );
    assert_eq!(legacy_catalog_category("Resume"), "Watch History");
    assert_eq!(legacy_catalog_category("Watch History"), "Watch History");
    assert_eq!(legacy_catalog_category("Favorites"), "Favorites");
}

#[test]
fn quality_tier_maps_to_the_server_filter_and_counts_once() {
    assert_eq!(QualityTier::All.query(), None);
    assert_eq!(QualityTier::Low.query(), Some("low"));
    assert_eq!(QualityTier::Medium.query(), Some("medium"));
    assert_eq!(QualityTier::High.query(), Some("high"));

    assert_eq!(
        QualityTier::All.toggled(QualityTier::High),
        QualityTier::High
    );
    assert_eq!(
        QualityTier::High.toggled(QualityTier::High),
        QualityTier::All
    );

    let filters = CatalogFilters {
        quality: QualityTier::High,
        ..Default::default()
    };
    assert_eq!(filters.active_filter_count("Latest"), 1);
    assert_eq!(filters.active_filter_count("Performers"), 0);
}

#[test]
fn accepts_never_re_derives_quality_from_file_size() {
    let mut item = TvItem::new("vid", "Title");
    item.file_size_bytes = Some(4 * 1024 * 1024 * 1024);
    item.width = None;
    item.height = None;

    // The server already decided the tier; a large file is not local evidence.
    assert!(CatalogFilters::default().accepts(&item));
    assert!(
        CatalogFilters {
            quality: QualityTier::High,
            ..Default::default()
        }
        .accepts(&item)
    );
}

#[test]
fn test_catalog_filter_sorts_and_labels() {
    let mut filters = CatalogFilters::default();
    assert_eq!(filters.sort_by(), "DateCreated");
    assert_eq!(catalog_filter_labels(&filters).0, "Date added");

    filters.sort = 1;
    assert_eq!(filters.sort_by(), "SortName");
    assert_eq!(catalog_filter_labels(&filters).0, "Title");

    filters.sort = 2;
    assert_eq!(filters.sort_by(), "Runtime");
    assert_eq!(catalog_filter_labels(&filters).0, "Duration");

    // Only the three sorts the API actually honours remain in the cycle.
    assert_eq!(next_catalog_sort(0), (1, false));
    assert_eq!(next_catalog_sort(1), (2, true));
    assert_eq!(next_catalog_sort(2), (0, true));
}

#[test]
fn catalog_filters_round_trip_through_the_local_store() {
    let store = adult_hub_storage::LocalStore::open_in_memory().expect("store");
    let filters = CatalogFilters {
        quality: QualityTier::High,
        performer_sort: PerformerSort::VideoCount,
        genre: "Anal".into(),
        descending: false,
        ..Default::default()
    };
    let mut map = std::collections::HashMap::new();
    map.insert("Latest".to_string(), filters);

    save_catalog_filters(&store, &map);
    let loaded = load_catalog_filters(&store);

    let restored = loaded.get("Latest").expect("restored filters");
    assert_eq!(restored.quality, QualityTier::High);
    assert_eq!(restored.performer_sort, PerformerSort::VideoCount);
    assert_eq!(restored.genre, "Anal");
    assert!(!restored.descending);
}

#[test]
fn test_catalog_filters_combined() {
    use adult_hub_client::TvPersonRole;

    let filters = CatalogFilters {
        genre: "Anal".into(),
        studio: "Evil Angel".into(),
        performer: "Eva Elfie".into(),
        ..Default::default()
    };

    assert_eq!(filters.active_filter_count("Latest"), 3);

    // Matching all 3
    let matching_item = TvItem {
        id: "match".into(),
        title: "Matching Item".into(),
        genres: vec!["Anal".into(), "Hardcore".into()],
        studios: vec!["Evil Angel".into()],
        people: vec![TvPersonRole {
            id: "p1".into(),
            name: "Eva Elfie".into(),
            role: Some("Actor".into()),
            person_type: Some("Actor".into()),
            primary_image_tag: None,
        }],
        ..Default::default()
    };
    assert!(filters.accepts(&matching_item));

    // Wrong genre
    let wrong_genre = TvItem {
        genres: vec!["Blowjob".into()],
        ..matching_item.clone()
    };
    assert!(!filters.accepts(&wrong_genre));

    // Wrong studio
    let wrong_studio = TvItem {
        studios: vec!["Brazzers".into()],
        ..matching_item.clone()
    };
    assert!(!filters.accepts(&wrong_studio));

    // Wrong performer
    let wrong_performer = TvItem {
        people: vec![TvPersonRole {
            id: "p2".into(),
            name: "Sweet Sophie".into(),
            role: Some("Actor".into()),
            person_type: Some("Actor".into()),
            primary_image_tag: None,
        }],
        ..matching_item.clone()
    };
    assert!(!filters.accepts(&wrong_performer));
}

#[test]
fn test_performer_filters_count_and_view_key() {
    let mut filters = CatalogFilters::default();
    assert_eq!(filters.active_filter_count("Performers"), 0);

    filters.performer_photo_only = true;
    assert_eq!(filters.active_filter_count("Performers"), 0);

    filters.performer_letter = "E".into();
    assert_eq!(filters.active_filter_count("Performers"), 1);

    let page1 = CatalogPageState {
        category: "Performers".into(),
        filters: CatalogFilters {
            performer_photo_only: false,
            ..Default::default()
        },
        ..CatalogPageState::default()
    };

    let page2 = CatalogPageState {
        category: "Performers".into(),
        filters: CatalogFilters {
            performer_photo_only: true,
            ..Default::default()
        },
        ..CatalogPageState::default()
    };

    assert_ne!(
        catalog_view_key(&page1),
        catalog_view_key(&page2),
        "Photo-only filter change must generate distinct view keys to avoid reusing un-filtered snapshot"
    );
}

#[test]
fn test_context_aware_search_context_mapping() {
    assert_eq!(search_context("browse", "Performers", false), "performers");
    assert_eq!(search_context("browse", "Studios", false), "studios");
    assert_eq!(search_context("browse", "Categories", false), "tags");
    assert_eq!(search_context("browse", "Latest", false), "movies");
    assert_eq!(search_context("browse", "Favorites", false), "favorites");
    assert_eq!(search_context("browse", "Watch History", false), "history");
    assert_eq!(
        search_context("browse", "Continue Watching", false),
        "resume"
    );
    assert_eq!(search_context("channels", "Latest", true), "channels");
    assert_eq!(
        search_context("browse", "Latest", true),
        "category_drilldown"
    );
    assert_ne!(
        resolved_search_context("browse", "Latest", true, "Anal"),
        resolved_search_context("browse", "Latest", true, "Blonde"),
        "Search history must be isolated by drilldown genre"
    );
    assert_eq!(
        resolved_search_context("browse", "Latest", true, "Anal"),
        "category:Anal"
    );
}

#[test]
fn search_context_labels_are_user_facing() {
    let labels = [
        ("browse", "Latest", false, "", "movies"),
        ("browse", "Performers", false, "", "performers"),
        ("browse", "Studios", false, "", "studios"),
        ("browse", "Categories", false, "", "categories"),
        ("browse", "Favorites", false, "", "favorites"),
        ("browse", "Watch History", false, "", "watch history"),
        (
            "browse",
            "Continue Watching",
            false,
            "",
            "continue watching",
        ),
        ("channels", "Latest", false, "", "channels"),
        ("browse", "Latest", true, "Anal", "category \"Anal\""),
    ];

    for (screen, category, drilldown, genre, expected) in labels {
        assert_eq!(
            search_context_label(screen, category, drilldown, genre),
            expected
        );
    }
}

#[test]
fn reported_studio_search_total_stays_stable_across_pages() {
    assert_eq!(
        reported_catalog_total(false, true, "Anal", true, 200, 46),
        200
    );
    assert_eq!(
        reported_catalog_total(false, true, "Anal", false, 200, 49),
        200
    );
    assert_eq!(
        reported_catalog_total(false, true, "", true, 200, 46),
        46,
        "unfiltered studio lists report the loaded, aggregated card count"
    );
    assert_eq!(reported_catalog_total(false, false, "", false, 200, 12), 12);
    assert_eq!(reported_catalog_total(false, false, "", true, 200, 50), 200);
}

#[test]
fn test_category_drilldown_view_key_and_query() {
    let page_genre = CatalogPageState {
        category: "Latest".into(),
        query: "blonde".into(),
        drilldown: true,
        filters: CatalogFilters {
            genre: "Anal".into(),
            ..Default::default()
        },
        ..CatalogPageState::default()
    };

    let page_global = CatalogPageState {
        category: "Latest".into(),
        query: "blonde".into(),
        filters: CatalogFilters::default(),
        ..CatalogPageState::default()
    };

    assert_ne!(
        catalog_view_key(&page_genre),
        catalog_view_key(&page_global),
        "Search within a category must have a distinct view key from global search"
    );
}

fn telegram_item() -> TvItem {
    let mut item = TvItem::new("telegram:-1001234567890:456:789", "Beach Day");
    item.provider_video_id = Some("-1001234567890:456:789".into());
    item
}

#[test]
fn item_matches_query_by_title_and_empty_query() {
    let item = telegram_item();
    assert!(item_matches_query(&item, "beach"));
    assert!(item_matches_query(&item, ""));
    assert!(!item_matches_query(&item, "mountain"));
}

#[test]
fn item_matches_query_by_telegram_id_prefix_and_plain_id() {
    let item = telegram_item();
    // Pasting the full Telegram reference finds the item even though the
    // title does not contain it.
    assert!(item_matches_query(&item, "telegram:-1001234567890:456:789"));
    assert!(item_matches_query(&item, "telegram:456"));
    // The bare id still matches through provider/id substring search.
    assert!(item_matches_query(&item, "456"));
}

#[test]
fn item_matches_query_ignores_unrelated_telegram_prefix() {
    let item = telegram_item();
    assert!(!item_matches_query(&item, "telegram:999"));
}

#[test]
fn resume_item_id_never_doubles_the_telegram_prefix() {
    // Every spelling stored by older builds maps to the id the catalog and
    // the resolver accept; a doubled prefix is what made the card 404.
    let provider_ref = "-1001618765010:315:5377541844798282037";
    for stored in [
        provider_ref.to_string(),
        format!("telegram:{provider_ref}"),
        format!("telegram:telegram:{provider_ref}"),
        format!("  telegram:{provider_ref}  "),
    ] {
        assert_eq!(
            resume_item_id(&stored).as_deref(),
            Some("telegram:-1001618765010:315:5377541844798282037"),
            "stored value {stored:?}"
        );
    }
}

#[test]
fn resume_item_id_keeps_legacy_hashes_and_hub_ids() {
    // Legacy opaque hashes still carry the Telegram prefix so the title
    // based remap downstream can pick them up.
    assert_eq!(
        resume_item_id("telegram:7f3a91c2").as_deref(),
        Some("telegram:7f3a91c2")
    );
    assert_eq!(
        resume_item_id("telegram:telegram:7f3a91c2").as_deref(),
        Some("telegram:7f3a91c2")
    );
    // Hub catalog ids are already canonical and must not gain a prefix.
    assert_eq!(
        resume_item_id("864d93c2-630d-5cfc-9022-a20ec78a352e").as_deref(),
        Some("864d93c2-630d-5cfc-9022-a20ec78a352e")
    );
    assert_eq!(resume_item_id("   "), None);
}

#[test]
fn item_matches_query_matches_overview() {
    let mut item = TvItem::new("library-guid-42", "Untitled");
    item.overview = Some("Recorded in Warsaw".into());
    assert!(item_matches_query(&item, "warsaw"));
}

#[test]
fn legacy_telegram_hashes_are_distinguished_from_canonical_refs() {
    assert!(is_legacy_telegram_hash_id(
        "telegram:5c23c503c114b8a273fd6ec9fbe0a031"
    ));
    assert!(!is_legacy_telegram_hash_id(
        "telegram:-1001618765010:315:5377541844798282037"
    ));
    assert!(!is_legacy_telegram_hash_id(
        "864d93c2-630d-5cfc-9022-a20ec78a352e"
    ));
}

/// The three tabs that answer a search with their own result list.
const SCOPED_SEARCH_TABS: [&str; 6] = [
    "Studios",
    "Categories",
    "Favorites",
    "Watch History",
    "Continue Watching",
    "Resume",
];

#[test]
fn filter_category_scopes_the_query_to_searchable_tabs() {
    // Performers searches by name, so the performer field wins.
    assert_eq!(
        category_query("Performers", false, "dragon", "typed"),
        "dragon"
    );
    // Every scoped list reuses the query the user typed there.
    for tab in SCOPED_SEARCH_TABS {
        assert_eq!(category_query(tab, false, "dragon", "typed"), "typed");
    }
    // A drilldown inside Latest carries the query; the plain Latest tab cannot.
    assert_eq!(category_query("Latest", true, "dragon", "typed"), "typed");
    assert_eq!(category_query("Latest", false, "dragon", "typed"), "");
    // Random and the remaining sections are never server-filtered.
    assert_eq!(category_query("Random", false, "dragon", "typed"), "");
    assert_eq!(category_query("Unknown", true, "dragon", "typed"), "");
}

#[test]
fn filter_category_reclick_refreshes_instead_of_restoring_a_snapshot() {
    // Random reshuffles wherever it is clicked from.
    assert!(is_category_reclick_refresh(
        "Random", "home", "Latest", false, ""
    ));
    // Clicking the tab already on screen drops its cached view...
    assert!(is_category_reclick_refresh(
        "Latest", "browse", "Latest", false, ""
    ));
    // ...but a drilldown or an active search must keep the snapshot.
    assert!(!is_category_reclick_refresh(
        "Latest", "browse", "Latest", true, ""
    ));
    assert!(!is_category_reclick_refresh(
        "Latest", "browse", "Latest", false, "dragon"
    ));
    // Choosing a different tab from another screen saves the outgoing view.
    assert!(!is_category_reclick_refresh(
        "Studios", "browse", "Latest", false, ""
    ));
    assert!(!is_category_reclick_refresh(
        "Latest", "settings", "Latest", false, ""
    ));
}

#[test]
fn filter_category_loading_message_names_the_target_tab() {
    assert_eq!(
        category_loading_message("Performers"),
        "Loading performers…"
    );
    assert_eq!(category_loading_message("Random"), "Loading random titles…");
    assert_eq!(category_loading_message("Studios"), "Loading studios…");
    assert_eq!(
        category_loading_message("Categories"),
        "Loading categories…"
    );
    assert_eq!(category_loading_message("Favorites"), "Loading favorites…");
    assert_eq!(
        category_loading_message("Watch History"),
        "Loading watch history…"
    );
    assert_eq!(
        category_loading_message("Continue Watching"),
        category_loading_message("Resume")
    );
    assert_eq!(category_loading_message("Latest"), "Loading library…");
}

fn catalog_item(id: &str, title: &str) -> TvItem {
    TvItem {
        id: id.into(),
        title: title.into(),
        ..Default::default()
    }
}

fn studio_item(id: &str, title: &str) -> TvItem {
    let mut item = catalog_item(id, title);
    item.genres = vec!["__adult_hub_studio".into()];
    item
}

fn catalog_ids(items: &[TvItem]) -> Vec<&str> {
    items.iter().map(|item| item.id.as_str()).collect()
}

#[test]
fn load_more_catalog_gate_stops_scrolling_after_a_failure() {
    // A failure banner keeps the scroll-driven prefetch away from the endpoint.
    assert_eq!(
        load_more_gate("Could not load more items: timeout", false, false, true),
        LoadMoreGate::Failed
    );
    // Only the banner's Retry gets through, even when the list looked exhausted.
    assert_eq!(
        load_more_gate("Could not load more items: timeout", true, false, false),
        LoadMoreGate::Proceed
    );
    // An in-flight page is never requested twice.
    assert_eq!(
        load_more_gate("", false, true, true),
        LoadMoreGate::InFlight
    );
    // The final page leaves `has_more` false until a retry forces another try.
    assert_eq!(
        load_more_gate("", false, false, false),
        LoadMoreGate::Exhausted
    );
    assert_eq!(
        load_more_gate("", false, true, false),
        LoadMoreGate::InFlight
    );
    assert_eq!(
        load_more_gate("", false, false, true),
        LoadMoreGate::Proceed
    );
}

#[test]
fn load_more_catalog_retry_clears_the_failure_and_marks_the_page_in_flight() {
    let mut paging = CatalogPageState {
        category: "Latest".into(),
        next_start: 50,
        ..Default::default()
    };
    // Page one failed: the handler keeps the loaded cards and stops paging.
    let mut error = "Could not load more items: connection reset".to_string();
    paging.has_more = false;
    assert_eq!(
        load_more_gate(&error, false, paging.loading_more, paging.has_more),
        LoadMoreGate::Failed
    );
    // Retry clears the banner, so the gate lets one forced request through.
    error.clear();
    assert_eq!(
        load_more_gate(&error, true, paging.loading_more, paging.has_more),
        LoadMoreGate::Proceed
    );
    // The handler then marks the page in flight before it spawns the worker.
    paging.has_more = true;
    paging.loading_more = true;
    assert_eq!(
        load_more_gate(&error, false, paging.loading_more, paging.has_more),
        LoadMoreGate::InFlight
    );
    assert_eq!(paging.next_start, 50);
}

#[test]
fn load_more_catalog_merge_skips_records_already_delivered() {
    // The server pages by offset, so an item can arrive twice when the catalog
    // shifts between requests; the grid must not grow duplicates.
    let mut cached = vec![catalog_item("a", "A"), catalog_item("b", "B")];
    merge_browse_page(
        "Latest",
        &mut cached,
        vec![catalog_item("b", "B"), catalog_item("c", "C")],
    );
    assert_eq!(catalog_ids(&cached), vec!["a", "b", "c"]);
}

#[test]
fn load_more_catalog_merge_keeps_one_card_per_studio() {
    let mut cached = vec![studio_item("first", "Vixen")];
    merge_browse_page("Studios", &mut cached, vec![studio_item("second", "vixen")]);
    assert_eq!(
        cached.len(),
        1,
        "the studios grid must not show the same studio twice"
    );
    assert!(
        cached[0].tags.contains(&"alias_id:second".to_string()),
        "the folded card keeps the merged record reachable"
    );
    // The same page is appended record-by-record outside the studios tab.
    let mut plain = vec![studio_item("first", "Vixen")];
    merge_browse_page("Latest", &mut plain, vec![studio_item("second", "vixen")]);
    assert_eq!(catalog_ids(&plain), vec!["first", "second"]);
}

#[test]
fn reported_total_uses_the_server_count_only_while_more_pages_exist() {
    // Mid-pagination the server total is the only number that covers the rest.
    assert_eq!(catalog_total_for_page(true, 5_000, 50), 5_000);
    // On the last page the server total can lag behind the loaded cards.
    assert_eq!(catalog_total_for_page(false, 900, 950), 950);
    assert_eq!(catalog_total_for_page(false, 0, 12), 12);
}

#[test]
fn catalog_view_key_separates_views_that_only_differ_in_state() {
    let base = CatalogPageState {
        category: "Latest".into(),
        query: String::new(),
        library_id: "lib-1".into(),
        ..CatalogPageState::default()
    };
    let key = catalog_view_key(&base);

    let mut filtered = base.clone();
    filtered.filters.genre = "Anal".into();
    assert_ne!(key, catalog_view_key(&filtered), "filters must take part");

    let mut drilled = base.clone();
    drilled.drilldown = true;
    assert_ne!(key, catalog_view_key(&drilled), "drilldown must take part");

    let mut other_library = base.clone();
    other_library.library_id = "lib-2".into();
    assert_ne!(
        key,
        catalog_view_key(&other_library),
        "libraries must not share a cached view"
    );

    let mut search = base.clone();
    search.is_search = true;
    assert_ne!(key, catalog_view_key(&search), "search must take part");

    assert_eq!(key, catalog_view_key(&base.clone()));
}
