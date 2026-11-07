//! Turns the panel's window into a non-activating panel, the kind of window menu bar apps use.
//! It opens over full-screen apps on whichever Space is showing, and takes clicks without
//! making Foxkit the active app, which would otherwise switch to Foxkit's own Space.

use std::cell::RefCell;
use std::ffi::{c_void, CStr};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::sel;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask, NSScreen, NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};

const CLASS_NAME: &CStr = c"FoxkitPanel";
// Tauri's window class adds 8 bytes of its own state to NSWindow. The panel class keeps the
// same space, so the window's memory still matches its class after the change.
type WindowState = u64;
static CONVERTED: AtomicBool = AtomicBool::new(false);
const NONACTIVATING_PANEL: usize = 1 << 7;
const CAN_JOIN_ALL_SPACES: usize = 1 << 0;
const STATIONARY: usize = 1 << 4;
const IGNORES_CYCLE: usize = 1 << 6;
const FULL_SCREEN_AUXILIARY: usize = 1 << 8;
const POP_UP_MENU_LEVEL: isize = 101;

extern "C-unwind" fn yes(_: &AnyObject, _: Sel) -> Bool {
    Bool::YES
}

extern "C-unwind" fn no(_: &AnyObject, _: Sel) -> Bool {
    Bool::NO
}

/// An NSPanel that can take keyboard focus without a title bar.
fn panel_class() -> &'static AnyClass {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        let superclass = AnyClass::get(c"NSPanel").expect("AppKit has NSPanel");
        let mut builder =
            ClassBuilder::new(CLASS_NAME, superclass).expect("the class is registered once");
        builder.add_ivar::<WindowState>(c"foxkitWindowState");
        // SAFETY: the signatures match these methods, which take no arguments and return BOOL.
        unsafe {
            builder.add_method(
                sel!(canBecomeKeyWindow),
                yes as extern "C-unwind" fn(_, _) -> _,
            );
            builder.add_method(
                sel!(canBecomeMainWindow),
                no as extern "C-unwind" fn(_, _) -> _,
            );
        }
        builder.register();
    });
    AnyClass::get(CLASS_NAME).expect("registered above")
}

/// Converts the window, and returns whether it did. Call once, on the main thread, with the
/// panel's NSWindow. If the window's class isn't the size this expects, as after a change in
/// Tauri, it's left alone: the panel still works, only not over full-screen apps.
///
/// # Safety
/// `window` must be a live NSWindow.
pub unsafe fn convert(window: *mut c_void) -> bool {
    let window = &*(window as *const AnyObject);
    let panel = panel_class();
    if window.class().instance_size() != panel.instance_size() {
        return false;
    }
    AnyObject::set_class(window, panel);
    let mask: usize = msg_send![window, styleMask];
    let _: () = msg_send![window, setStyleMask: mask | NONACTIVATING_PANEL];
    let behaviour = CAN_JOIN_ALL_SPACES | STATIONARY | IGNORES_CYCLE | FULL_SCREEN_AUXILIARY;
    let _: () = msg_send![window, setCollectionBehavior: behaviour];
    let _: () = msg_send![window, setLevel: POP_UP_MENU_LEVEL];
    let _: () = msg_send![window, setFloatingPanel: true];
    let _: () = msg_send![window, setHidesOnDeactivate: false];
    CONVERTED.store(true, Ordering::Relaxed);
    true
}

pub fn converted() -> bool {
    CONVERTED.load(Ordering::Relaxed)
}

/// Shows the panel and gives it focus without activating Foxkit.
///
/// # Safety
/// `window` must be the converted panel's NSWindow, used on the main thread.
pub unsafe fn show(window: *mut c_void) {
    let window = &*(window as *const AnyObject);
    let none: *const AnyObject = std::ptr::null();
    let _: () = msg_send![window, makeKeyAndOrderFront: none];
    highlight_icon(true);
}

const GAP: f64 = 6.0;

thread_local! {
    // The menu bar icon's button, shown pressed while the panel is open, as macOS's own
    // menu bar icons are. Only ever used on the main thread.
    static ICON_BUTTON: RefCell<Option<Retained<NSView>>> = const { RefCell::new(None) };
}

fn highlight_icon(on: bool) {
    ICON_BUTTON.with(|button| {
        if let Some(button) = button.borrow().as_ref() {
            let button: &AnyObject = button;
            // SAFETY: the status bar button is an NSButton, which has highlight:.
            let responds: bool = unsafe { msg_send![button, respondsToSelector: sel!(highlight:)] };
            if responds {
                let _: () = unsafe { msg_send![button, highlight: on] };
            }
        }
    });
}

/// Shows the menu bar icon as no longer pressed. Call on the main thread when the panel hides.
pub fn release_icon() {
    highlight_icon(false);
}

/// The menu bar icon's frame in screen points: the status bar window the click came from.
/// Failing that, a point at the mouse, just under the menu bar of its screen.
fn icon_frame(mtm: MainThreadMarker) -> NSRect {
    let status_window = NSApplication::sharedApplication(mtm)
        .currentEvent()
        .and_then(|event| event.window(mtm))
        .filter(|window| {
            window
                .class()
                .name()
                .to_bytes()
                .ends_with(b"StatusBarWindow")
        });
    ICON_BUTTON.with(|button| {
        *button.borrow_mut() = status_window
            .as_ref()
            .and_then(|window| window.contentView());
    });
    let from_click = status_window.map(|window| window.frame());
    from_click.unwrap_or_else(|| {
        let mouse = NSEvent::mouseLocation();
        let below_menu_bar = screen_at(mtm, mouse)
            .map(|screen| screen.visibleFrame())
            .map_or(mouse.y, |visible| visible.origin.y + visible.size.height);
        NSRect::new(NSPoint::new(mouse.x, below_menu_bar), NSSize::new(0.0, 0.0))
    })
}

fn screen_at(mtm: MainThreadMarker, point: NSPoint) -> Option<Retained<NSScreen>> {
    let screens = NSScreen::screens(mtm);
    let found = screens.iter().find(|screen| {
        let frame = screen.frame();
        (frame.origin.x..=frame.origin.x + frame.size.width).contains(&point.x)
            && (frame.origin.y..=frame.origin.y + frame.size.height).contains(&point.y)
    });
    found.or_else(|| NSScreen::mainScreen(mtm))
}

/// Places the panel under the menu bar icon, centred on it and kept inside its screen. Works
/// in screen points, so it's right on every screen whatever its pixel density.
///
/// # Safety
/// `window` must be the panel's NSWindow.
pub unsafe fn place_under_icon(window: *mut c_void) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let window = &*(window as *const NSWindow);
    let icon = icon_frame(mtm);
    let centre = NSPoint::new(
        icon.origin.x + icon.size.width / 2.0,
        icon.origin.y + icon.size.height / 2.0,
    );
    let width = window.frame().size.width;
    let mut x = centre.x - width / 2.0;
    if let Some(screen) = screen_at(mtm, centre) {
        let frame = screen.frame();
        let left = frame.origin.x + GAP;
        let right = frame.origin.x + frame.size.width - GAP - width;
        x = x.clamp(left, right.max(left));
    }
    window.setFrameTopLeftPoint(NSPoint::new(x, icon.origin.y - GAP));
}

/// Calls `on_click` for every mouse click in other apps, on the desktop or in the menu bar
/// outside Foxkit's icon, so the panel can close the way menu bar panels do. Mouse clicks
/// need no permission to watch this way. Clicks on Foxkit's own windows and icon aren't
/// included.
pub fn watch_outside_clicks(on_click: impl Fn() + 'static) {
    let mask =
        NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown;
    let handler = RcBlock::new(move |_event: NonNull<NSEvent>| on_click());
    // AppKit calls the handler on the main thread.
    let monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &handler);
    // Watched for as long as Foxkit runs.
    std::mem::forget(monitor);
}
