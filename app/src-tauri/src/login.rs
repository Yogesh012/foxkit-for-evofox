//! Launch at login. On macOS, Foxkit is an Open at Login item, listed in System Settings under
//! General → Login Items, where it can also be turned off.

use auto_launch::{AutoLaunch, AutoLaunchBuilder};

// Given at login where the system passes it, as on Linux, to start in the menu bar only.
pub const HIDDEN_ARG: &str = "--hidden";

fn launcher() -> Result<AutoLaunch, String> {
    let mut builder = AutoLaunchBuilder::new();
    builder.set_app_name("Foxkit");
    #[cfg(target_os = "macos")]
    builder.set_macos_launch_mode(auto_launch::MacOSLaunchMode::SMAppService);
    #[cfg(not(target_os = "macos"))]
    {
        let path = std::env::current_exe().map_err(|error| error.to_string())?;
        builder
            .set_app_path(&path.to_string_lossy())
            .set_args(&[HIDDEN_ARG]);
    }
    builder
        .build()
        .map_err(|error| format!("Launch at login isn't available: {error}"))
}

pub fn is_enabled() -> bool {
    launcher()
        .and_then(|launcher| launcher.is_enabled().map_err(|error| error.to_string()))
        .unwrap_or(false)
}

/// Turns launch at login on or off, and returns whether it's on afterwards.
pub fn set(enabled: bool) -> Result<bool, String> {
    let launcher = launcher()?;
    let changed = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    changed.map_err(|error| format!("Couldn't change launch at login: {error}"))?;
    launcher.is_enabled().map_err(|error| error.to_string())
}

/// Whether Foxkit was opened at login, in which case it starts in the menu bar only.
pub fn opened_at_login() -> bool {
    std::env::args().any(|arg| arg == HIDDEN_ARG) || opened_as_login_item()
}

/// macOS marks the event that opens an app at login. Call while the app finishes launching.
#[cfg(target_os = "macos")]
fn opened_as_login_item() -> bool {
    use objc2_foundation::NSAppleEventManager;

    const OPEN_APPLICATION: u32 = u32::from_be_bytes(*b"oapp");
    const PROPERTY_DATA: u32 = u32::from_be_bytes(*b"prdt");
    const LAUNCHED_AS_LOGIN_ITEM: u32 = u32::from_be_bytes(*b"lgit");

    let manager = NSAppleEventManager::sharedAppleEventManager();
    let Some(event) = manager.currentAppleEvent() else {
        return false;
    };
    // SAFETY: eventID returns an AEEventID and paramDescriptorForKeyword: takes an
    // AEKeyword, both four-character codes.
    let event_id: u32 = unsafe { objc2::msg_send![&event, eventID] };
    let data: Option<objc2::rc::Retained<objc2_foundation::NSAppleEventDescriptor>> =
        unsafe { objc2::msg_send![&event, paramDescriptorForKeyword: PROPERTY_DATA] };
    event_id == OPEN_APPLICATION
        && data.is_some_and(|data| data.enumCodeValue() == LAUNCHED_AS_LOGIN_ITEM)
}

#[cfg(not(target_os = "macos"))]
fn opened_as_login_item() -> bool {
    false
}
