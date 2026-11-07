//! The menu bar icon and its panel, and the main window coming and going. Foxkit keeps
//! running in the menu bar when the main window closes, and shows a Dock icon only while the
//! window is open.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use tauri::image::Image;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    ActivationPolicy, AppHandle, LogicalSize, Manager, PhysicalPosition, Rect, WebviewWindow,
    WindowEvent,
};

pub const MAIN: &str = "main";
pub const PANEL: &str = "panel";
const PANEL_WIDTH: f64 = 320.0;
const PANEL_MAX_HEIGHT: f64 = 640.0;
const TRAY_ICON: &[u8] = include_bytes!("../icons/tray@2x.png");
// Clicking the icon while the panel is open first hides the panel, as it loses focus. A click
// this soon after that means close, not open again.
const REOPEN_GUARD: Duration = Duration::from_millis(300);
// Space between the menu bar and the panel, and between the panel and the screen's edges.
const GAP: f64 = 6.0;

#[derive(Default)]
pub struct PanelHidden(Mutex<Option<Instant>>);

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    TrayIconBuilder::with_id("foxkit")
        .icon(Image::from_bytes(TRAY_ICON)?)
        .icon_as_template(true)
        .tooltip("Foxkit")
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle_panel(tray.app_handle(), rect);
            }
        })
        .build(app)?;
    if let Some(panel) = app.get_webview_window(PANEL) {
        #[cfg(target_os = "macos")]
        if let Ok(window) = panel.ns_window() {
            // SAFETY: the panel's live NSWindow, during setup, on the main thread.
            if unsafe { crate::floating_panel::convert(window) } {
                // It never becomes part of an active app, so macOS doesn't tell it when focus
                // moves elsewhere. A click anywhere outside Foxkit closes it instead.
                let handle = app.clone();
                crate::floating_panel::watch_outside_clicks(move || {
                    let open = handle
                        .get_webview_window(PANEL)
                        .is_some_and(|panel| panel.is_visible().unwrap_or(false));
                    if open {
                        hide_panel(&handle);
                    }
                });
            } else {
                eprintln!(
                    "The panel stays an ordinary window, so it won't show over full-screen apps."
                );
            }
        }
        let handle = app.clone();
        panel.on_window_event(move |event| {
            if let WindowEvent::Focused(false) = event {
                hide_panel(&handle);
            }
        });
    }
    if let Some(main) = app.get_webview_window(MAIN) {
        let handle = app.clone();
        main.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                hide_main(&handle);
            }
        });
    }
    Ok(())
}

fn toggle_panel(app: &AppHandle, icon: Rect) {
    let Some(panel) = app.get_webview_window(PANEL) else {
        return;
    };
    if panel.is_visible().unwrap_or(false) {
        hide_panel(app);
        return;
    }
    let hidden_at = *app
        .state::<PanelHidden>()
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if hidden_at.is_some_and(|at| at.elapsed() < REOPEN_GUARD) {
        return;
    }
    #[cfg(target_os = "macos")]
    if let Some(window) = crate::floating_panel::converted()
        .then(|| panel.ns_window().ok())
        .flatten()
    {
        // SAFETY: the panel's NSWindow, converted in install, on the main thread, which tray
        // clicks arrive on.
        unsafe {
            crate::floating_panel::place_under_icon(window);
            crate::floating_panel::show(window);
        }
        return;
    }
    place_panel(app, &panel, icon);
    let _ = panel.show();
    let _ = panel.set_focus();
}

/// Centres the panel under the icon, kept inside the screen.
fn place_panel(app: &AppHandle, panel: &WebviewWindow, icon: Rect) {
    let Ok(scale) = panel.scale_factor() else {
        return;
    };
    let position = icon.position.to_physical::<f64>(scale);
    let size = icon.size.to_physical::<f64>(scale);
    let width = PANEL_WIDTH * scale;
    let gap = GAP * scale;
    let mut x = position.x + size.width / 2.0 - width / 2.0;
    if let Ok(Some(monitor)) = app.monitor_from_point(position.x, position.y) {
        let left = f64::from(monitor.position().x) + gap;
        let right = left - gap + f64::from(monitor.size().width) - gap - width;
        x = x.clamp(left, right.max(left));
    }
    let y = position.y + size.height + gap;
    let _ = panel.set_position(PhysicalPosition::new(x, y));
}

fn hide_panel(app: &AppHandle) {
    if let Some(panel) = app.get_webview_window(PANEL) {
        let _ = panel.hide();
    }
    #[cfg(target_os = "macos")]
    let _ = app.run_on_main_thread(crate::floating_panel::release_icon);
    *app.state::<PanelHidden>()
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
}

pub fn show_main(app: &AppHandle) {
    let _ = app.set_activation_policy(ActivationPolicy::Regular);
    if let Some(main) = app.get_webview_window(MAIN) {
        let _ = main.unminimize();
        let _ = main.show();
        let _ = main.set_focus();
    }
}

pub fn hide_main(app: &AppHandle) {
    if let Some(main) = app.get_webview_window(MAIN) {
        let _ = main.hide();
    }
    let _ = app.set_activation_policy(ActivationPolicy::Accessory);
}

/// Opens the main window from the panel.
#[tauri::command]
pub fn open_main(app: AppHandle) {
    hide_panel(&app);
    show_main(&app);
}

/// Sizes the panel to its content, which changes as devices come and go.
#[tauri::command]
pub fn fit_panel(app: AppHandle, height: f64) {
    if let Some(panel) = app.get_webview_window(PANEL) {
        let height = height.clamp(1.0, PANEL_MAX_HEIGHT);
        let _ = panel.set_size(LogicalSize::new(PANEL_WIDTH, height));
    }
}

/// Quits. The bars are stopped on the way out, which restores the keyboard's lighting.
#[tauri::command]
pub fn quit(app: AppHandle) {
    app.exit(0);
}
