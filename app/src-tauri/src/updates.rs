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
// After a failed check, such as while offline, try again sooner than the next day.
const RETRY_AFTER: Duration = Duration::from_secs(15 * 60);

/// The newer version found by the last check, if any.
#[derive(Default)]
pub struct Available(Mutex<Option<Update>>);

/// How far an install has got, while one is going.
#[derive(Default)]
pub struct Installing(Mutex<Option<Progress>>);

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum Progress {
    Downloading { received: u64, total: Option<u64> },
    Installing,
}

fn set_progress(app: &AppHandle, progress: Option<Progress>) {
    *app.state::<Installing>()
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = progress;
}

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
            let wait = match tauri::async_runtime::block_on(check(&app)) {
                Ok(()) => CHECK_EVERY,
                Err(problem) => {
                    eprintln!("{problem}");
                    RETRY_AFTER
                }
            };
            thread::sleep(wait);
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

fn waiting(available: &Available) -> Option<UpdateInfo> {
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

/// The version waiting to be installed, if there is one.
#[tauri::command(async)]
pub fn available_update(available: tauri::State<'_, Available>) -> Option<UpdateInfo> {
    waiting(&available)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    update: Option<UpdateInfo>,
    current_version: String,
}

/// Checks GitHub now, for the Check for updates buttons.
#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<CheckResult, String> {
    check(&app).await?;
    Ok(CheckResult {
        update: waiting(&app.state::<Available>()),
        current_version: app.package_info().version.to_string(),
    })
}

/// How far the install has got, for the Update buttons to show.
#[tauri::command(async)]
pub fn update_progress(installing: tauri::State<'_, Installing>) -> Option<Progress> {
    *installing.0.lock().unwrap_or_else(PoisonError::into_inner)
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
    set_progress(
        &app,
        Some(Progress::Downloading {
            received: 0,
            total: None,
        }),
    );
    let mut received = 0u64;
    let (downloading, installing) = (app.clone(), app.clone());
    let installed = update
        .download_and_install(
            move |chunk, total| {
                received += chunk as u64;
                set_progress(
                    &downloading,
                    Some(Progress::Downloading { received, total }),
                );
            },
            move || set_progress(&installing, Some(Progress::Installing)),
        )
        .await;
    if let Err(error) = installed {
        set_progress(&app, None);
        return Err(format!("Couldn't install the update: {error}"));
    }
    app.restart();
}
