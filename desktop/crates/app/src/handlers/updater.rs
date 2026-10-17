use crate::MainWindow;
use crate::context::AppContext;
use crate::updater;
use slint::ComponentHandle;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn install_stage5_handlers(ui: &MainWindow, ctx: &AppContext) {
    let player = ctx.player.clone();
    let local_store = ctx.local_store.clone();

    let app_version = env!("CARGO_PKG_VERSION");
    ui.set_app_version(app_version.into());

    let initial_auto_update = local_store
        .get_setting("auto_update_enabled")
        .ok()
        .flatten()
        .map(|v| v == "true")
        .unwrap_or(true);
    ui.set_auto_update_enabled(initial_auto_update);

    let pending_update = Arc::new(Mutex::new(
        None::<(updater::UpdateManifest, updater::UpdatePackage)>,
    ));
    let downloaded_installer = Arc::new(Mutex::new(None::<PathBuf>));

    // Callback: Toggle Auto Update
    {
        let weak = ui.as_weak();
        let store = local_store.clone();
        ui.on_toggle_auto_update(move || {
            if let Some(ui) = weak.upgrade() {
                let current = ui.get_auto_update_enabled();
                let new_val = !current;
                ui.set_auto_update_enabled(new_val);
                let _ = store.set_setting(
                    "auto_update_enabled",
                    if new_val { "true" } else { "false" },
                );
                ui.set_update_status(if new_val {
                    "Automatic update checks enabled".into()
                } else {
                    "Automatic update checks disabled".into()
                });
            }
        });
    }

    // Helper closure: run update check
    let check_updates_logic = {
        let weak = ui.as_weak();
        let pending = pending_update.clone();
        let _downloaded = downloaded_installer.clone();
        Arc::new(move |silent_if_uptodate: bool| {
            if let Some(ui) = weak.upgrade() {
                ui.set_update_status("Checking for updates...".into());
            }
            let weak_thread = weak.clone();
            let pending_thread = pending.clone();

            std::thread::spawn(move || {
                let release_url = std::env::var("ADULT_HUB_UPDATE_URL").unwrap_or_else(|_| {
                    "https://raw.githubusercontent.com/adult-hub/adult-hub-desktop/main/releases/latest.json".to_string()
                });
                let sig_url = format!("{release_url}.sig");

                let client = match reqwest::blocking::Client::builder().build() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak_thread.upgrade() {
                                ui.set_update_status(format!("Update check error: {e}").into());
                            }
                        });
                        return;
                    }
                };

                let manifest_res = client.get(&release_url).send();
                let sig_res = client.get(&sig_url).send();

                match (manifest_res, sig_res) {
                    (Ok(m_resp), Ok(s_resp))
                        if m_resp.status().is_success() && s_resp.status().is_success() =>
                    {
                        let m_bytes = m_resp.bytes().unwrap_or_default();
                        let s_bytes = s_resp.bytes().unwrap_or_default();

                        match updater::parse_and_verify_manifest(
                            &m_bytes,
                            &s_bytes,
                            &updater::EMBEDDED_RELEASE_PUBLIC_KEY,
                            updater::DEFAULT_ALLOWED_HOSTS,
                        ) {
                            Ok(manifest) => {
                                let curr_ver = env!("CARGO_PKG_VERSION");
                                let is_newer =
                                    updater::is_newer_version(&manifest.version, curr_ver);
                                let is_appimage = std::env::var("APPIMAGE").is_ok();
                                let target_os = if cfg!(windows) { "windows" } else { "linux" };
                                let pkg = updater::select_best_package(
                                    &manifest.packages,
                                    target_os,
                                    "x86_64",
                                    is_appimage,
                                )
                                .cloned();
                                let manifest_clone = manifest.clone();

                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = weak_thread.upgrade() {
                                        if is_newer && let Some(p) = pkg {
                                            ui.set_update_available(true);
                                            ui.set_update_ready(false);
                                            ui.set_update_version(
                                                manifest_clone.version.clone().into(),
                                            );
                                            ui.set_update_release_notes(
                                                manifest_clone
                                                    .release_notes
                                                    .clone()
                                                    .unwrap_or_default()
                                                    .into(),
                                            );
                                            let size_mb = p.size_bytes / (1024 * 1024);
                                            ui.set_update_status(
                                                format!(
                                                    "Update available: v{} ({} MB)",
                                                    manifest_clone.version, size_mb
                                                )
                                                .into(),
                                            );
                                            *pending_thread.lock().unwrap() =
                                                Some((manifest_clone, p));
                                        } else if !silent_if_uptodate {
                                            ui.set_update_available(false);
                                            ui.set_update_status(
                                                format!(
                                                    "You are on the latest version (v{}).",
                                                    curr_ver
                                                )
                                                .into(),
                                            );
                                        } else {
                                            ui.set_update_status("".into());
                                        }
                                    }
                                });
                            }
                            Err(e) => {
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = weak_thread.upgrade() {
                                        ui.set_update_status(
                                            format!("Manifest verification failed: {e}").into(),
                                        );
                                    }
                                });
                            }
                        }
                    }
                    _ => {
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak_thread.upgrade() {
                                if !silent_if_uptodate {
                                    ui.set_update_status(
                                        "Up to date or no release manifest reachable.".into(),
                                    );
                                } else {
                                    ui.set_update_status("".into());
                                }
                            }
                        });
                    }
                }
            });
        })
    };

    // Callback: Check for updates (manual)
    {
        let check = check_updates_logic.clone();
        ui.on_check_for_updates(move || {
            check(false);
        });
    }

    // Callback: Start Update Download
    {
        let weak = ui.as_weak();
        let pending = pending_update.clone();
        let downloaded = downloaded_installer.clone();
        ui.on_start_update_download(move || {
            let pkg_opt = pending.lock().unwrap().as_ref().map(|(_, p)| p.clone());
            if let Some(package) = pkg_opt {
                if let Some(ui) = weak.upgrade() {
                    ui.set_update_downloading(true);
                    ui.set_update_status("Downloading update package...".into());
                }
                let weak_thread = weak.clone();
                let downloaded_thread = downloaded.clone();
                std::thread::spawn(move || {
                    let weak_prog = weak_thread.clone();
                    let res =
                        updater::download_and_verify_package(&package, move |bytes, total| {
                            if total > 0 {
                                let prog = (bytes as f32) / (total as f32);
                                let mb_down = (bytes as f64) / (1024.0 * 1024.0);
                                let mb_total = (total as f64) / (1024.0 * 1024.0);
                                let weak_cb = weak_prog.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = weak_cb.upgrade() {
                                        ui.set_update_progress(prog);
                                        ui.set_update_status(
                                            format!(
                                                "Downloading: {:.1}% ({:.1} MB / {:.1} MB)",
                                                prog * 100.0,
                                                mb_down,
                                                mb_total
                                            )
                                            .into(),
                                        );
                                    }
                                });
                            }
                        });

                    match res {
                        Ok(verified_path) => {
                            *downloaded_thread.lock().unwrap() = Some(verified_path);
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = weak_thread.upgrade() {
                                    ui.set_update_downloading(false);
                                    ui.set_update_ready(true);
                                    ui.set_update_status(
                                        "Update verified! Ready to install.".into(),
                                    );
                                }
                            });
                        }
                        Err(e) => {
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = weak_thread.upgrade() {
                                    ui.set_update_downloading(false);
                                    ui.set_update_status(format!("Download failed: {e}").into());
                                }
                            });
                        }
                    }
                });
            }
        });
    }

    // Callback: Install Update
    {
        let weak = ui.as_weak();
        let pending = pending_update.clone();
        let downloaded = downloaded_installer.clone();
        let player = player.clone();
        ui.on_install_update(move || {
            let pkg_type = pending
                .lock()
                .unwrap()
                .as_ref()
                .map(|(_, p)| p.package_type.clone());
            let path_opt = downloaded.lock().unwrap().clone();
            if let (Some(ptype), Some(path)) = (pkg_type, path_opt) {
                if let Some(player) = &player {
                    let _ = player.stop();
                }
                match updater::launch_installation(&ptype, &path, false) {
                    Ok(()) => {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_update_status("Update launched. Closing application...".into());
                        }
                    }
                    Err(e) => {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_update_status(
                                format!("Failed to launch installation: {e}").into(),
                            );
                        }
                    }
                }
            }
        });
    }

    // Auto-check on startup (after 3 seconds) if enabled
    if initial_auto_update {
        let check = check_updates_logic;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            check(true);
        });
    }
}
