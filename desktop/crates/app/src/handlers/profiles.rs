use crate::MainWindow;
use crate::catalog::{filter_catalog_items, tv_item_to_card};
use crate::context::AppContext;
use adult_hub_client::{GetItemsQuery, TvItem};
use slint::ComponentHandle;
use std::rc::Rc;

pub fn install_stage7_handlers(ui: &MainWindow, ctx: &AppContext) {
    let local_store = ctx.local_store.clone();
    let credential_store = ctx.credential_store.clone();
    let jellyfin = ctx.jellyfin.clone();
    let resolver = ctx.resolver.clone();
    let cached_catalog = ctx.cached_catalog.clone();

    // 1. Open Performer Profile Dialog
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();
        let resolver = resolver.clone();

        ui.on_open_performer_profile(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            let name_str = name.to_string();
            ui.set_performer_name(name.clone());
            ui.set_performer_bio("".into());
            ui.set_performer_nationality("".into());
            ui.set_performer_birth("".into());
            ui.set_performer_aliases("".into());

            // Initial catalog match for filmography
            let matching: Vec<TvItem> = cached_catalog
                .lock()
                .unwrap()
                .iter()
                .filter(|i| {
                    i.people
                        .iter()
                        .any(|p| p.name.eq_ignore_ascii_case(&name_str))
                })
                .cloned()
                .collect();
            let cards = matching.iter().map(tv_item_to_card).collect::<Vec<_>>();
            let model = Rc::new(slint::VecModel::from(cards));
            ui.set_performer_items(model.into());
            ui.set_performer_profile_visible(true);

            // Fetch performer bio & remote filmography asynchronously
            let weak = weak.clone();
            let local_store = local_store.clone();
            let credential_store = credential_store.clone();
            let jellyfin = jellyfin.clone();
            let resolver = resolver.clone();
            let p_name = name_str.clone();

            std::thread::spawn(move || {
                let res_url = local_store
                    .get_setting("resolver_base_url")
                    .ok()
                    .flatten()
                    .or_else(|| credential_store.get().ok().flatten().map(|s| s.server_url))
                    .unwrap_or_default();

                if !res_url.is_empty() {
                    if let Ok(bio_dto) = resolver.get_performer_bio(&res_url, &p_name) {
                        let weak2 = weak.clone();
                        slint::invoke_from_event_loop(move || {
                            let Some(ui) = weak2.upgrade() else { return };
                            if let Some(bio) = bio_dto.bio {
                                ui.set_performer_bio(bio.into());
                            }
                            if let Some(nat) = bio_dto.nationality {
                                ui.set_performer_nationality(nat.into());
                            }
                            if let Some(birth) = bio_dto.birth {
                                ui.set_performer_birth(birth.into());
                            }
                            if !bio_dto.aliases.is_empty() {
                                ui.set_performer_aliases(bio_dto.aliases.join(", ").into());
                            }
                        })
                        .ok();
                    }
                }

                // If logged in to Jellyfin, fetch person filmography
                if let Ok(Some(sess)) = credential_store.get() {
                    let q = GetItemsQuery {
                        person: Some(&p_name),
                        sort_by: "SortName",
                        limit: 50,
                        ..Default::default()
                    };
                    if let Ok(page) =
                        jellyfin.get_items(&sess.server_url, &sess.user_id, &sess.access_token, &q)
                    {
                        let filtered = filter_catalog_items(page.items, &local_store);
                        let cards = filtered.iter().map(tv_item_to_card).collect::<Vec<_>>();
                        let weak2 = weak.clone();
                        slint::invoke_from_event_loop(move || {
                            let Some(ui) = weak2.upgrade() else { return };
                            let model = Rc::new(slint::VecModel::from(cards));
                            ui.set_performer_items(model.into());
                        })
                        .ok();
                    }
                }
            });
        });
    }

    // 2. Close Performer Profile Dialog
    {
        let weak = ui.as_weak();
        ui.on_close_performer_profile(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_performer_profile_visible(false);
            }
        });
    }

    // 3. Open Studio Profile Dialog
    {
        let weak = ui.as_weak();
        let cached_catalog = cached_catalog.clone();
        let credential_store = credential_store.clone();
        let local_store = local_store.clone();
        let jellyfin = jellyfin.clone();

        ui.on_open_studio_profile(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            let studio_str = name.to_string();
            ui.set_studio_name(name.clone());

            let matching: Vec<TvItem> = cached_catalog
                .lock()
                .unwrap()
                .iter()
                .filter(|i| {
                    i.studios
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(&studio_str))
                })
                .cloned()
                .collect();
            let cards = matching.iter().map(tv_item_to_card).collect::<Vec<_>>();
            let model = Rc::new(slint::VecModel::from(cards));
            ui.set_studio_items(model.into());
            ui.set_studio_profile_visible(true);

            // Fetch studio releases from Jellyfin if connected
            let weak = weak.clone();
            let local_store = local_store.clone();
            let credential_store = credential_store.clone();
            let jellyfin = jellyfin.clone();
            let s_name = studio_str.clone();

            std::thread::spawn(move || {
                if let Ok(Some(sess)) = credential_store.get() {
                    let q = GetItemsQuery {
                        studio: Some(&s_name),
                        sort_by: "SortName",
                        limit: 50,
                        ..Default::default()
                    };
                    if let Ok(page) =
                        jellyfin.get_items(&sess.server_url, &sess.user_id, &sess.access_token, &q)
                    {
                        let filtered = filter_catalog_items(page.items, &local_store);
                        let cards = filtered.iter().map(tv_item_to_card).collect::<Vec<_>>();
                        let weak2 = weak.clone();
                        slint::invoke_from_event_loop(move || {
                            let Some(ui) = weak2.upgrade() else { return };
                            let model = Rc::new(slint::VecModel::from(cards));
                            ui.set_studio_items(model.into());
                        })
                        .ok();
                    }
                }
            });
        });
    }

    // 4. Close Studio Profile Dialog
    {
        let weak = ui.as_weak();
        ui.on_close_studio_profile(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_studio_profile_visible(false);
            }
        });
    }
}
