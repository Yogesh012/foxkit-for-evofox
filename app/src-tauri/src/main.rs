#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod banshee;
mod bars;
mod devices;
mod dpi_table;
mod ronin;

use std::fs;
use std::sync::{Mutex, PoisonError};
use std::thread;

use hidapi::{HidApi, HidDevice};
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

const BARS_SHORTCUT: (Modifiers, Code) = (Modifiers::CONTROL.union(Modifiers::ALT), Code::KeyM);
const STYLE_FILE: &str = "music-style.json";

// hidapi allows one instance per process, and each device takes one request at a time.
struct Hid(Mutex<HidApi>);

// At most one run of the bars, which holds the keyboard while it lasts.
#[derive(Default)]
struct Bars(Mutex<Option<bars::Running>>);

impl Bars {
    fn is_active(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|running| !running.is_finished())
    }

    fn stop(&self) -> Option<bars::Report> {
        let running = self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
        running.map(bars::Running::stop)
    }
}

// The keyboard kept open between checks, because it only sends its own reports to an open
// connection. Closed while the bars have the keyboard.
#[derive(Default)]
struct KeyboardWatch(Mutex<Option<(HidDevice, u16)>>);

impl KeyboardWatch {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(HidDevice, u16)>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// A problem with the shortcut, kept until the window picks it up, since nothing else shows it.
#[derive(Default)]
struct ShortcutProblem(Mutex<Option<String>>);

// The music style chosen last, which the next run starts on, including after a restart.
#[derive(Default)]
struct LastStyle(Mutex<bars::Style>);

fn load_style(app: &AppHandle) -> bars::Style {
    let saved = app
        .path()
        .app_config_dir()
        .ok()
        .and_then(|folder| fs::read_to_string(folder.join(STYLE_FILE)).ok());
    saved
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

// Losing this only means starting on the default style next time.
fn save_style(app: &AppHandle, style: bars::Style) {
    let (Ok(folder), Ok(text)) = (app.path().app_config_dir(), serde_json::to_string(&style))
    else {
        return;
    };
    let _ = fs::create_dir_all(&folder);
    let _ = fs::write(folder.join(STYLE_FILE), text);
}

#[tauri::command(async)]
fn read_devices(hid: tauri::State<'_, Hid>, bars: tauri::State<'_, Bars>) -> devices::Devices {
    let keyboard_busy = bars.is_active();
    let mut api = hid.0.lock().unwrap_or_else(PoisonError::into_inner);
    devices::read_all(&mut api, keyboard_busy)
}

#[tauri::command(async)]
fn change_mouse(
    hid: tauri::State<'_, Hid>,
    change: banshee::Change,
) -> Result<banshee::Status, String> {
    let mut api = hid.0.lock().unwrap_or_else(PoisonError::into_inner);
    devices::change_mouse(&mut api, change)
}

#[tauri::command(async)]
fn change_keyboard(
    hid: tauri::State<'_, Hid>,
    bars: tauri::State<'_, Bars>,
    watch: tauri::State<'_, KeyboardWatch>,
    change: ronin::Change,
) -> Result<ronin::Status, String> {
    if bars.is_active() {
        return Err("Stop the audio bars first.".to_string());
    }
    let status = {
        let mut api = hid.0.lock().unwrap_or_else(PoisonError::into_inner);
        devices::change_keyboard(&mut api, change)
    };
    // Reports queued before the change would put old values back in the window.
    if let Some((device, product_id)) = watch.lock().as_ref() {
        let _ = ronin::latest_reported(device, *product_id);
    }
    status
}

/// The newest status the keyboard has sent by itself since the last check, if any.
#[tauri::command(async)]
fn keyboard_report(
    hid: tauri::State<'_, Hid>,
    bars: tauri::State<'_, Bars>,
    watch: tauri::State<'_, KeyboardWatch>,
) -> Option<ronin::Status> {
    if bars.is_active() {
        return None;
    }
    let mut watching = watch.lock();
    if watching.is_none() {
        let mut api = hid.0.lock().unwrap_or_else(PoisonError::into_inner);
        *watching = devices::open_keyboard(&mut api).ok();
    }
    let (device, product_id) = watching.as_ref()?;
    match ronin::latest_reported(device, *product_id) {
        Ok(status) => status,
        // It was unplugged or changed connection, so open it again next time.
        Err(_) => {
            *watching = None;
            None
        }
    }
}

#[tauri::command(async)]
fn start_bars(
    hid: tauri::State<'_, Hid>,
    bars: tauri::State<'_, Bars>,
    watch: tauri::State<'_, KeyboardWatch>,
    style: tauri::State<'_, LastStyle>,
) -> Result<(), String> {
    begin_bars(&hid, &bars, &watch, &style)
}

fn begin_bars(
    hid: &Hid,
    bars: &Bars,
    watch: &KeyboardWatch,
    style: &LastStyle,
) -> Result<(), String> {
    let mut slot = bars.0.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.as_ref().is_some_and(|running| !running.is_finished()) {
        return Err("The audio bars are already running.".to_string());
    }
    *watch.lock() = None;
    let (device, product_id) = {
        let mut api = hid.0.lock().unwrap_or_else(PoisonError::into_inner);
        devices::open_keyboard(&mut api)?
    };
    let style = *style.0.lock().unwrap_or_else(PoisonError::into_inner);
    *slot = Some(bars::Running::start(device, product_id, style)?);
    Ok(())
}

#[tauri::command(async)]
fn bars_report(bars: tauri::State<'_, Bars>) -> Option<bars::Report> {
    bars.0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(bars::Running::report)
}

#[tauri::command(async)]
fn change_bars(
    app: AppHandle,
    bars: tauri::State<'_, Bars>,
    last_style: tauri::State<'_, LastStyle>,
    look: bars::Look,
) -> Result<(), String> {
    let slot = bars.0.lock().unwrap_or_else(PoisonError::into_inner);
    let running = slot
        .as_ref()
        .filter(|running| !running.is_finished())
        .ok_or("The audio bars have stopped.")?;
    running.set_look(look)?;
    let mut last = last_style.0.lock().unwrap_or_else(PoisonError::into_inner);
    if *last != look.style {
        *last = look.style;
        save_style(&app, look.style);
    }
    Ok(())
}

#[tauri::command(async)]
fn stop_bars(bars: tauri::State<'_, Bars>) -> Option<bars::Report> {
    bars.stop()
}

#[tauri::command(async)]
fn shortcut_problem(problem: tauri::State<'_, ShortcutProblem>) -> Option<String> {
    problem
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
}

/// Starts the bars, or stops them if they're running. The window finds out from the report.
fn toggle_bars(app: &AppHandle) {
    let bars = app.state::<Bars>();
    {
        let slot = bars.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(running) = slot.as_ref().filter(|running| !running.is_finished()) {
            running.request_stop();
            return;
        }
    }
    if let Err(problem) = begin_bars(
        &app.state::<Hid>(),
        &bars,
        &app.state::<KeyboardWatch>(),
        &app.state::<LastStyle>(),
    ) {
        *app.state::<ShortcutProblem>()
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(problem);
    }
}

fn main() {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(error) => {
            eprintln!("Couldn't start HID access: {error}");
            std::process::exit(1);
        }
    };
    #[cfg(target_os = "macos")]
    api.set_open_exclusive(false);

    // Prints what the window would show, for checking devices without opening it.
    if std::env::args().any(|arg| arg == "--status") {
        let mut api = api;
        let report = devices::read_all(&mut api, false);
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("status serializes")
        );
        return;
    }

    // Prints audio levels for this many seconds, without touching the keyboard.
    let args: Vec<String> = std::env::args().collect();
    if let Some(seconds) = args
        .iter()
        .position(|arg| arg == "--meter")
        .and_then(|index| args.get(index + 1))
    {
        let result = seconds
            .parse()
            .map_err(|_| "--meter needs a number of seconds".to_string())
            .and_then(bars::run_meter);
        if let Err(problem) = result {
            eprintln!("{problem}");
            std::process::exit(1);
        }
        return;
    }

    tauri::Builder::default()
        .manage(Hid(Mutex::new(api)))
        .manage(Bars::default())
        .manage(KeyboardWatch::default())
        .manage(ShortcutProblem::default())
        .manage(LastStyle::default())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        // Starting talks to the keyboard, which shouldn't hold up the window.
                        let app = app.clone();
                        thread::spawn(move || toggle_bars(&app));
                    }
                })
                .build(),
        )
        .setup(|app| {
            *app.state::<LastStyle>()
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = load_style(app.handle());
            let (modifiers, key) = BARS_SHORTCUT;
            if let Err(error) = app
                .global_shortcut()
                .register(Shortcut::new(Some(modifiers), key))
            {
                let problem = format!("Couldn't register the Ctrl+Option+M shortcut: {error}");
                *app.state::<ShortcutProblem>()
                    .0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(problem);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            read_devices,
            change_mouse,
            change_keyboard,
            keyboard_report,
            start_bars,
            bars_report,
            change_bars,
            stop_bars,
            shortcut_problem
        ])
        .build(tauri::generate_context!())
        .expect("the window should open")
        .run(|app, event| {
            // Closing the app ends the bars and puts the keyboard's lighting back.
            if let tauri::RunEvent::Exit = event {
                app.state::<Bars>().stop();
            }
        });
}
