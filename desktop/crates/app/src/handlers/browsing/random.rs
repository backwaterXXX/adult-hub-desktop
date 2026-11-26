use super::{CATALOG_PAGE_SIZE, CatalogError, CatalogFilters, CatalogPage};
use adult_hub_client::{AdultHubClient, AdultHubError, CatalogVideosQuery, TvItem};

pub(super) fn random_catalog_page(
    client: &AdultHubClient,
    filters: &CatalogFilters,
    page_index: usize,
) -> std::result::Result<CatalogPage, CatalogError> {
    let limit = CATALOG_PAGE_SIZE as u64;
    let map_error = |err: AdultHubError| {
        if err.is_unauthorized() {
            CatalogError::Unauthorized
        } else {
            CatalogError::Api(err.to_string())
        }
    };
    let build_query = |offset: u64| CatalogVideosQuery {
        offset: Some(offset),
        limit: Some(limit),
        // Ordering is discarded; a deterministic server order is required only
        // so pagination has an offset to sample from.
        sort: Some("latest"),
        direction: Some("desc"),
        channel_id: None,
        q: None,
        category: (!filters.genre.is_empty()).then_some(filters.genre.as_str()),
        tag: None,
        studio: (!filters.studio.is_empty()).then_some(filters.studio.as_str()),
        performer: (!filters.performer.is_empty()).then_some(filters.performer.as_str()),
        performer_id: None,
        min_duration: None,
        is_favorite: filters.favorites_only.then_some(true),
    };

    let probe = client.get_videos(&build_query(0)).map_err(map_error)?;
    let total_count = probe.total_count as usize;
    let offset = random_page_offset(total_count, CATALOG_PAGE_SIZE);
    let response = if offset == 0 {
        probe
    } else {
        client.get_videos(&build_query(offset)).map_err(map_error)?
    };

    let mut items = response
        .items
        .into_iter()
        .map(TvItem::from)
        .filter(|item| filters.accepts(item))
        .collect::<Vec<_>>();
    fastrand::shuffle(&mut items);

    let next_page = page_index + 1;
    // An empty page after the caller's retries means the active filters found
    // nothing in three random slices; stop rather than offer endless retries.
    let has_more =
        !items.is_empty() && random_page_has_more(next_page, total_count, CATALOG_PAGE_SIZE);
    Ok(CatalogPage {
        items,
        next_start: next_page,
        total_count,
        has_more,
        supplemental_total: 0,
    })
}

/// Largest offset the server can satisfy with one full random page.
pub(super) fn random_page_offset(total_count: usize, limit: usize) -> u64 {
    let max_offset = total_count.saturating_sub(limit);
    if max_offset == 0 {
        0
    } else {
        fastrand::u64(0..=max_offset as u64)
    }
}

pub(super) fn random_page_has_more(next_page: usize, total_count: usize, limit: usize) -> bool {
    let pages = total_count.div_ceil(limit.max(1)).max(1);
    next_page < pages
}
