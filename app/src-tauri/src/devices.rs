//! Finds each device's settings channel and reads its status.

use hidapi::{DeviceInfo, HidApi, HidDevice};
use serde::Serialize;

use crate::{banshee, ronin};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Devices {
    pub keyboard: Reading<ronin::Status>,
    pub mouse: Reading<banshee::Status>,
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Reading<T> {
    Connected {
        status: T,
    },
    /// Listed by the system, but this connection has no settings channel, as on Bluetooth.
    NoSettings {
        name: String,
    },
    NotFound,
    Failed {
        message: String,
    },
    /// Audio bars have the keyboard, and a status request would land between their frames.
    Busy,
}

pub fn read_all(api: &mut HidApi, keyboard_busy: bool) -> Devices {
    if let Err(error) = api.refresh_devices() {
        let message = error.to_string();
        return Devices {
            keyboard: Reading::Failed {
                message: message.clone(),
            },
            mouse: Reading::Failed { message },
        };
    }
    Devices {
        keyboard: if keyboard_busy {
            Reading::Busy
        } else {
            read_device(
                api,
                ronin::is_settings_channel,
                ronin::is_this_device,
                ronin::read_status,
            )
        },
        mouse: read_device(
            api,
            banshee::is_settings_channel,
            banshee::is_this_device,
            banshee::read_status,
        ),
    }
}

pub fn change_mouse(api: &mut HidApi, change: banshee::Change) -> Result<banshee::Status, String> {
    api.refresh_devices().map_err(|error| error.to_string())?;
    let info = api
        .device_list()
        .find(|info| banshee::is_settings_channel(info))
        .ok_or("The mouse's settings aren't available. Connect it with the USB cable.")?;
    let device = info
        .open_device(api)
        .map_err(|error| format!("Couldn't open the settings channel: {error}"))?;
    banshee::apply(&device, change)
}

pub fn change_keyboard(api: &mut HidApi, change: ronin::Change) -> Result<ronin::Status, String> {
    let (device, product_id) = open_keyboard(api)?;
    ronin::apply(&device, product_id, change)
}

pub fn open_keyboard(api: &mut HidApi) -> Result<(HidDevice, u16), String> {
    api.refresh_devices().map_err(|error| error.to_string())?;
    let info = api
        .device_list()
        .find(|info| ronin::is_settings_channel(info))
        .ok_or(
            "The keyboard's settings aren't available. Connect it with the cable or receiver.",
        )?;
    let device = info
        .open_device(api)
        .map_err(|error| format!("Couldn't open the settings channel: {error}"))?;
    Ok((device, info.product_id()))
}

fn read_device<T>(
    api: &HidApi,
    is_settings_channel: fn(&DeviceInfo) -> bool,
    is_this_device: fn(&DeviceInfo) -> bool,
    read_status: fn(&HidDevice, u16) -> Result<T, String>,
) -> Reading<T> {
    if let Some(info) = api.device_list().find(|info| is_settings_channel(info)) {
        let result = info
            .open_device(api)
            .map_err(|error| format!("Couldn't open the settings channel: {error}"))
            .and_then(|device| read_status(&device, info.product_id()));
        return match result {
            Ok(status) => Reading::Connected { status },
            Err(message) => Reading::Failed { message },
        };
    }
    match api.device_list().find(|info| is_this_device(info)) {
        Some(info) => Reading::NoSettings {
            name: info
                .product_string()
                .unwrap_or("Unnamed device")
                .to_string(),
        },
        None => Reading::NotFound,
    }
}
