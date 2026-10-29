//! Windows-only automatic update. At launch of a release build the app asks GitHub
//! Releases for `latest.json`, verifies the installer against the minisign key in
//! `tauri.conf.json`, installs it and restarts. macOS builds do not include it.

use tauri_plugin_updater::UpdaterExt;

pub fn start(app: tauri::AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        if let Err(error) = update_and_restart(&app).await {
            eprintln!("[update] Aggiornamento automatico fallito: {error}");
        }
    });
}

async fn update_and_restart(app: &tauri::AppHandle) -> tauri_plugin_updater::Result<()> {
    let Some(update) = app.updater()?.check().await? else {
        return Ok(());
    };
    eprintln!("[update] Installazione della versione {}", update.version);
    update.download_and_install(|_, _| {}, || {}).await?;
    app.restart();
}
