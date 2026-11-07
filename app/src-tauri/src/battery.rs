//! Battery levels for the panel. USB and 2.4 GHz come from each device's own status. Bluetooth
//! comes from macOS's list of accessory power sources, the one its battery widget shows.

use serde::Serialize;

use crate::devices::Reading;
use crate::{banshee, ronin};

pub const BLUETOOTH: &str = "Bluetooth";

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// A device as the panel shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum PanelDevice {
    Connected {
        connection: &'static str,
        battery: Option<Battery>,
    },
    NotConnected,
    Failed {
        message: String,
    },
}

/// One accessory from macOS's power source list.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Accessory {
    pub name: String,
    pub battery: Battery,
}

/// What macOS lists for one power source, before it's checked.
#[derive(Debug, Default)]
struct Description {
    name: Option<String>,
    current: Option<i64>,
    max: Option<i64>,
    state: Option<String>,
    is_charging: Option<bool>,
}

fn accessory(description: Description) -> Option<Accessory> {
    let name = description.name.filter(|name| !name.is_empty())?;
    let max = description.max.filter(|max| *max > 0)?;
    let current = description.current?.clamp(0, max);
    let percent = u8::try_from((current * 100 + max / 2) / max).ok()?;
    let charging =
        description.is_charging == Some(true) || description.state.as_deref() == Some("AC Power");
    Some(Accessory {
        name,
        battery: Battery { percent, charging },
    })
}

/// The keyboard's battery from its own status. Through the receiver, status can arrive before
/// the keyboard has reported its battery, and then reads 0, so 0 means not known yet. A
/// keyboard at 0% would be off.
pub fn keyboard_status_battery(status: &ronin::Status) -> Option<Battery> {
    (status.battery > 0).then_some(Battery {
        percent: status.battery,
        charging: status.charging,
    })
}

fn find<'a>(accessories: &'a [Accessory], name_part: &str) -> Option<&'a Accessory> {
    accessories
        .iter()
        .find(|accessory| accessory.name.contains(name_part))
}

/// Keyboard: its own status when the settings channel answers, the bars' last report while
/// they have it, and macOS's accessory list on Bluetooth.
pub fn keyboard(
    reading: &Reading<ronin::Status>,
    during_bars: Option<(&'static str, Option<Battery>)>,
    accessories: &[Accessory],
) -> PanelDevice {
    match reading {
        Reading::Connected { status } => PanelDevice::Connected {
            connection: status.connection,
            battery: keyboard_status_battery(status),
        },
        Reading::Busy => match during_bars {
            Some((connection, battery)) => PanelDevice::Connected {
                connection,
                battery,
            },
            None => PanelDevice::NotConnected,
        },
        other => bluetooth(other, find(accessories, ronin::BLUETOOTH_NAME)),
    }
}

pub fn mouse(reading: &Reading<banshee::Status>, accessories: &[Accessory]) -> PanelDevice {
    match reading {
        Reading::Connected { status } => PanelDevice::Connected {
            connection: "USB",
            battery: Some(Battery {
                percent: status.battery_percent,
                charging: status.charging,
            }),
        },
        other => bluetooth(other, find(accessories, banshee::BLUETOOTH_NAME)),
    }
}

fn bluetooth<T>(reading: &Reading<T>, accessory: Option<&Accessory>) -> PanelDevice {
    match (reading, accessory) {
        (_, Some(accessory)) => PanelDevice::Connected {
            connection: BLUETOOTH,
            battery: Some(accessory.battery),
        },
        (Reading::NoSettings { .. }, None) => PanelDevice::Connected {
            connection: BLUETOOTH,
            battery: None,
        },
        (Reading::Failed { message }, None) => PanelDevice::Failed {
            message: message.clone(),
        },
        _ => PanelDevice::NotConnected,
    }
}

#[cfg(target_os = "macos")]
pub fn accessories() -> Vec<Accessory> {
    macos::descriptions()
        .into_iter()
        .filter_map(accessory)
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn accessories() -> Vec<Accessory> {
    Vec::new()
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{c_char, c_int, c_void, CStr};

    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;

    use super::Description;

    // Accessories only, as pmset -g accps lists them.
    const SOURCE_FOR_ACCESSORIES: c_int = 4;
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
    const COPY_BY_TYPE: &CStr = c"IOPSCopyPowerSourcesByType";

    type CopyByType = unsafe extern "C" fn(c_int) -> CFTypeRef;

    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFArrayRef;
        fn IOPSGetPowerSourceDescription(blob: CFTypeRef, source: CFTypeRef) -> CFDictionaryRef;
    }

    /// The accessory list comes from a function IOKit exports but doesn't document, so it's
    /// looked up when needed, and its absence only means no Bluetooth battery.
    pub fn descriptions() -> Vec<Description> {
        // SAFETY: dlsym takes a nul-terminated name and returns null if it's missing.
        let symbol = unsafe { dlsym(RTLD_DEFAULT, COPY_BY_TYPE.as_ptr()) };
        if symbol.is_null() {
            return Vec::new();
        }
        // SAFETY: IOKit's function has this signature, as pmset uses it.
        let copy_by_type: CopyByType = unsafe { std::mem::transmute(symbol) };
        // SAFETY: a Copy function, so the result is owned here.
        let blob = unsafe { copy_by_type(SOURCE_FOR_ACCESSORIES) };
        if blob.is_null() {
            return Vec::new();
        }
        let blob = unsafe { CFType::wrap_under_create_rule(blob) };
        // SAFETY: blob is a power source snapshot, and the list is a Copy result.
        let list = unsafe { IOPSCopyPowerSourcesList(blob.as_CFTypeRef()) };
        if list.is_null() {
            return Vec::new();
        }
        let list: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(list) };
        list.iter()
            .filter_map(|source| {
                // SAFETY: a Get function, so the dictionary belongs to the snapshot.
                let dictionary = unsafe {
                    IOPSGetPowerSourceDescription(blob.as_CFTypeRef(), source.as_CFTypeRef())
                };
                (!dictionary.is_null()).then(|| {
                    let dictionary: CFDictionary<CFString, CFType> =
                        unsafe { CFDictionary::wrap_under_get_rule(dictionary) };
                    describe(&dictionary)
                })
            })
            .collect()
    }

    fn describe(dictionary: &CFDictionary<CFString, CFType>) -> Description {
        let value = |key: &str| {
            dictionary
                .find(CFString::new(key))
                .map(|value| (*value).clone())
        };
        let string = |key| value(key).and_then(|value| value.downcast::<CFString>());
        let number = |key| {
            value(key)
                .and_then(|value| value.downcast::<CFNumber>())
                .and_then(|number| number.to_i64())
        };
        Description {
            name: string("Name").map(|name| name.to_string()),
            current: number("Current Capacity"),
            max: number("Max Capacity"),
            state: string("Power Source State").map(|state| state.to_string()),
            is_charging: value("Is Charging")
                .and_then(|value| value.downcast::<CFBoolean>())
                .map(bool::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(name: &str, current: i64, state: &str) -> Description {
        Description {
            name: Some(name.to_string()),
            current: Some(current),
            max: Some(100),
            state: Some(state.to_string()),
            is_charging: None,
        }
    }

    fn mouse_at(percent: u8) -> Accessory {
        Accessory {
            name: "Banshee-2 M1".to_string(),
            battery: Battery {
                percent,
                charging: false,
            },
        }
    }

    #[test]
    fn accessory_reads_percent_and_charging() {
        let on_battery = accessory(listed("Banshee-2 M1", 73, "Battery Power")).unwrap();
        assert_eq!(on_battery, mouse_at(73));
        let charging = accessory(listed("EvoFox Ronin TKL-1", 40, "AC Power")).unwrap();
        assert!(charging.battery.charging);
    }

    #[test]
    fn accessory_scales_other_maximums() {
        let mut description = listed("Mouse", 3, "Battery Power");
        description.max = Some(4);
        assert_eq!(accessory(description).unwrap().battery.percent, 75);
    }

    #[test]
    fn accessory_needs_a_name_and_maximum() {
        assert_eq!(accessory(Description::default()), None);
        let mut description = listed("Mouse", 50, "Battery Power");
        description.max = Some(0);
        assert_eq!(accessory(description), None);
    }

    #[test]
    fn mouse_on_bluetooth_uses_the_accessory_list() {
        let reading = Reading::<banshee::Status>::NoSettings {
            name: "Banshee-2 M1".to_string(),
        };
        assert_eq!(
            mouse(&reading, &[mouse_at(73)]),
            PanelDevice::Connected {
                connection: BLUETOOTH,
                battery: Some(mouse_at(73).battery),
            }
        );
        assert_eq!(
            mouse(&reading, &[]),
            PanelDevice::Connected {
                connection: BLUETOOTH,
                battery: None,
            }
        );
    }

    #[test]
    fn mouse_missing_everywhere_is_not_connected() {
        assert_eq!(mouse(&Reading::NotFound, &[]), PanelDevice::NotConnected);
    }

    fn keyboard_status(battery: u8) -> ronin::Status {
        ronin::Status {
            connection: "2.4 GHz receiver",
            battery,
            charging: false,
            firmware: "0.0.8".to_string(),
            effect: "Constant".to_string(),
            effect_number: Some(8),
            effect_names: &[],
            colour: 0,
            brightness: 5,
            speed: 3,
            direction: 0,
        }
    }

    #[test]
    fn keyboard_battery_of_zero_is_not_known_yet() {
        let reading = Reading::Connected {
            status: keyboard_status(0),
        };
        assert_eq!(
            keyboard(&reading, None, &[]),
            PanelDevice::Connected {
                connection: "2.4 GHz receiver",
                battery: None,
            }
        );
        let reading = Reading::Connected {
            status: keyboard_status(64),
        };
        assert!(matches!(
            keyboard(&reading, None, &[]),
            PanelDevice::Connected {
                battery: Some(Battery { percent: 64, .. }),
                ..
            }
        ));
    }

    #[test]
    fn keyboard_during_bars_uses_their_report() {
        let battery = Some(Battery {
            percent: 90,
            charging: true,
        });
        assert_eq!(
            keyboard(&Reading::Busy, Some(("USB", battery)), &[]),
            PanelDevice::Connected {
                connection: "USB",
                battery,
            }
        );
    }
}
