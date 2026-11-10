//! Updates from GitHub releases. Foxkit checks shortly after launch and then once a day, and
//! the panel and main window offer the new version. Nothing is installed until asked.

use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

// Give launch a moment before reaching out to GitHub.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(10);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The newer version found by the last check, if any.
#[derive(Default)]
pub struct Available(Mutex<Option<Update>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    version: String,
    current_version: String,
}

pub fn start_checking(app: AppHandle) {
    thread::spawn(move || {
        thread::sleep(FIRST_CHECK_AFTER);
        loop {
            // Offline or GitHub unreachable: try again at the next check.
            if let Err(problem) = tauri::async_runtime::block_on(check(&app)) {
                eprintln!("{problem}");
            }
            thread::sleep(CHECK_EVERY);
        }
    });
}

async fn check(app: &AppHandle) -> Result<(), String> {
    let update = app
        .updater()
        .map_err(|error| format!("Couldn't set up update checks: {error}"))?
        .check()
        .await
        .map_err(|error| format!("Couldn't check for updates: {error}"))?;
    *app.state::<Available>()
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = update;
    Ok(())
}

/// The version waiting to be installed, if there is one.
#[tauri::command(async)]
pub fn available_update(available: tauri::State<'_, Available>) -> Option<UpdateInfo> {
    available
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(|update| UpdateInfo {
            version: update.version.clone(),
            current_version: update.current_version.clone(),
        })
}

/// Downloads and installs the waiting version, then restarts Foxkit into it. The bars stop
/// first, so the keyboard's lighting is put back before Foxkit closes.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    let update = app
        .state::<Available>()
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or("There's no update waiting.")?;
    app.state::<crate::Bars>().stop();
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|error| format!("Couldn't install the update: {error}"))?;
    app.restart();
}
