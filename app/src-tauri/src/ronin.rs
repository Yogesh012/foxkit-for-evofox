//! Ronin TKL (FP batch) settings channel.
//!
//! Only the status request, the lighting command, lights off, Music Rhythm mode and live
//! frames can be built here, so the save, macro and factory reset commands can't be sent.
//! Lighting changes go through one request per choice in the window, and are never
//! streamed. Only frames are streamed, because they aren't kept by the keyboard.

use std::thread::sleep;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidDevice};
use serde::{Deserialize, Serialize};

const VID: u16 = 0x1A2C;
const PID_USB: u16 = 0xA157;
const PID_RECEIVER: u16 = 0x8FF0;
const USAGE_PAGE: u16 = 0xFF02;
const USAGE: u16 = 0x0002;
const MAGIC: [u8; 4] = [0xAA, 0x55, 0xCC, 0x33];
const CMD_STATUS: u8 = 0x0E;
const CMD_LIGHTS_OFF: u8 = 0x05;
const CMD_EFFECT: u8 = 0x07;
const PACKET: usize = 64;
const STATUS_BYTES: usize = 19;
const REPLY_WAIT: Duration = Duration::from_secs(1);
const READ_TIMEOUT_MS: i32 = 100;
const CHECK_ATTEMPTS: usize = 5;
const CHECK_WAIT: Duration = Duration::from_millis(150);
const CMD_FRAME_FIRST: u8 = 0xF0;
const FRAME_PAGES: usize = 6;
const PAGE_BYTES: usize = 54;
pub const FRAME_BYTES: usize = FRAME_PAGES * PAGE_BYTES;
const PAGE_DELAY: Duration = Duration::from_millis(1);
const EFFECT_CUSTOM: u8 = 0x14;
const EFFECT_MUSIC: u8 = 0xAA;
pub const LEVEL_MAX: u8 = 5;
pub const DIRECTION_MAX: u8 = 1;
pub const COLOUR_MAX: u8 = 7;

/// Effects 1 to 20, in order.
const EFFECT_NAMES: [&str; 20] = [
    "Flow",
    "Starry",
    "Ripple",
    "Laser",
    "Spectrum",
    "Kaleidoscope",
    "Constant",
    "Breath",
    "Cross Flow",
    "Float",
    "Snow",
    "Jianghua",
    "Snake",
    "Tornado",
    "Impact",
    "Rain",
    "Windmill",
    "Sine",
    "Horse",
    "Cross",
];

/// RGB for each of the 108 LED slots.
pub type Frame = [u8; FRAME_BYTES];

/// A change asked for in the window.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Change {
    Lighting {
        effect: u8,
        brightness: u8,
        speed: u8,
        direction: u8,
        colour: u8,
    },
    LightsOff,
}

impl Change {
    /// The same change on the next effect, going back to the first after the last.
    pub fn with_next_effect(self) -> Self {
        match self {
            Self::Lighting {
                effect,
                brightness,
                speed,
                direction,
                colour,
            } => Self::Lighting {
                effect: effect % EFFECT_NAMES.len() as u8 + 1,
                brightness,
                speed,
                direction,
                colour,
            },
            Self::LightsOff => self,
        }
    }
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub connection: &'static str,
    pub battery: u8,
    pub charging: bool,
    pub firmware: String,
    pub effect: String,
    /// 1 to 20. None for Custom and Music Rhythm, which these controls can't keep.
    pub effect_number: Option<u8>,
    pub effect_names: &'static [&'static str],
    pub colour: u8,
    pub brightness: u8,
    pub speed: u8,
    pub direction: u8,
}

pub fn is_settings_channel(info: &DeviceInfo) -> bool {
    info.vendor_id() == VID && info.usage_page() == USAGE_PAGE && info.usage() == USAGE
}

pub fn is_this_device(info: &DeviceInfo) -> bool {
    info.vendor_id() == VID
}

fn packet(cmd: u8, args: &[u8]) -> [u8; PACKET + 1] {
    let mut request = [0; PACKET + 1];
    // Report ID 0, which hidapi needs as a leading byte.
    request[1..5].copy_from_slice(&MAGIC);
    request[5] = cmd;
    request[6..6 + args.len()].copy_from_slice(args);
    request
}

fn status_request() -> [u8; PACKET + 1] {
    packet(CMD_STATUS, &[])
}

fn change_request(change: Change) -> Result<[u8; PACKET + 1], String> {
    match change {
        Change::LightsOff => Ok(packet(CMD_LIGHTS_OFF, &[0x01])),
        Change::Lighting {
            effect,
            brightness,
            speed,
            direction,
            colour,
        } => {
            // Effect 21 is off and 23 is custom, and those need their own commands.
            if !(1..=EFFECT_NAMES.len() as u8).contains(&effect) {
                return Err(format!("Effect {effect} isn't one of the 20 effects."));
            }
            if !(1..=LEVEL_MAX).contains(&brightness) || !(1..=LEVEL_MAX).contains(&speed) {
                return Err(format!(
                    "Brightness and speed have to be from 1 to {LEVEL_MAX}."
                ));
            }
            if direction > DIRECTION_MAX || colour > COLOUR_MAX {
                return Err("That direction or colour isn't available.".to_string());
            }
            Ok(packet(
                CMD_EFFECT,
                &[effect - 1, brightness - 1, speed - 1, direction, colour],
            ))
        }
    }
}

fn music_mode_request() -> [u8; PACKET + 1] {
    packet(CMD_EFFECT, &[EFFECT_MUSIC])
}

fn frame_requests(frame: &Frame) -> impl Iterator<Item = [u8; PACKET + 1]> + '_ {
    frame
        .chunks_exact(PAGE_BYTES)
        .zip(CMD_FRAME_FIRST..)
        .map(|(page, cmd)| packet(cmd, page))
}

fn parse_status(packet: &[u8], product_id: u16) -> Option<Status> {
    if packet.len() < STATUS_BYTES || packet[..4] != MAGIC || packet[4] != CMD_STATUS {
        return None;
    }
    let effect_number = packet[14]
        .checked_add(1)
        .filter(|number| (1..=EFFECT_NAMES.len() as u8).contains(number));
    Some(Status {
        connection: match product_id {
            PID_USB => "USB",
            PID_RECEIVER => "2.4 GHz receiver",
            _ => "Unknown",
        },
        battery: packet[5],
        charging: packet[7] != 0,
        firmware: format!(
            "{}.{}.{}",
            packet[12] >> 4,
            packet[12] & 15,
            u16::from(packet[13] >> 4) * 10 + u16::from(packet[13] & 15)
        ),
        effect: match (effect_number, packet[14]) {
            (Some(number), _) => EFFECT_NAMES[usize::from(number - 1)].to_string(),
            (None, EFFECT_CUSTOM) => "Custom".to_string(),
            (None, EFFECT_MUSIC) => "Music Rhythm".to_string(),
            (None, other) => format!("Unknown ({other})"),
        },
        effect_number,
        effect_names: &EFFECT_NAMES,
        colour: packet[15],
        // The keyboard stores these counting from 0.
        brightness: packet[16].saturating_add(1),
        speed: packet[17].saturating_add(1),
        direction: packet[18],
    })
}

fn matches_change(status: &Status, change: Change) -> bool {
    match change {
        // Off can't be read back: the status keeps its last values.
        Change::LightsOff => true,
        Change::Lighting {
            effect,
            brightness,
            speed,
            direction,
            colour,
        } => {
            status.effect_number == Some(effect)
                && status.brightness == brightness
                && status.speed == speed
                && status.direction == direction
                && status.colour == colour
        }
    }
}

pub fn read_status(device: &HidDevice, product_id: u16) -> Result<Status, String> {
    device
        .write(&status_request())
        .map_err(|error| format!("Status request failed: {error}"))?;
    let sent = Instant::now();
    let mut packet = [0; PACKET];
    while sent.elapsed() < REPLY_WAIT {
        let size = device
            .read_timeout(&mut packet, READ_TIMEOUT_MS)
            .map_err(|error| format!("Status read failed: {error}"))?;
        if let Some(status) = parse_status(&packet[..size], product_id) {
            return Ok(status);
        }
    }
    Err("The keyboard didn't answer. Press Fn+5 if you're using the cable, or Fn+4 for the receiver.".to_string())
}

/// Sends one change once, checks the keyboard took it, and returns the new status.
pub fn apply(device: &HidDevice, product_id: u16, change: Change) -> Result<Status, String> {
    let request = change_request(change)?;
    device
        .write(&request)
        .map_err(|error| format!("Keyboard write failed: {error}"))?;
    for _ in 0..CHECK_ATTEMPTS {
        sleep(CHECK_WAIT);
        let status = read_status(device, product_id)?;
        if matches_change(&status, change) {
            return Ok(status);
        }
    }
    Err("The keyboard didn't report the new lighting.".to_string())
}

/// The command that puts back this status's lighting. Custom and Music Rhythm have none.
pub fn restore_change(status: &Status) -> Result<Change, String> {
    let effect = status.effect_number.ok_or_else(|| {
        format!(
            "The keyboard is on {}. Pick an effect first, so it can be put back afterwards.",
            status.effect
        )
    })?;
    let change = Change::Lighting {
        effect,
        brightness: status.brightness,
        speed: status.speed,
        direction: status.direction,
        colour: status.colour,
    };
    change_request(change)?;
    Ok(change)
}

pub fn enter_music_mode(device: &HidDevice) -> Result<(), String> {
    device
        .write(&music_mode_request())
        .map(|_| ())
        .map_err(|error| format!("Keyboard write failed: {error}"))
}

/// Sends one frame and returns how many of its pages failed to send.
pub fn send_frame(device: &HidDevice, frame: &Frame) -> u32 {
    let mut failed = 0;
    for request in frame_requests(frame) {
        if device.write(&request).is_err() {
            failed += 1;
        }
        sleep(PAGE_DELAY);
    }
    failed
}

/// The newest status the keyboard has sent by itself, without asking for one.
pub fn latest_reported(device: &HidDevice, product_id: u16) -> Result<Option<Status>, String> {
    let mut packet = [0; PACKET];
    let mut latest = None;
    loop {
        let size = device
            .read_timeout(&mut packet, 0)
            .map_err(|error| format!("Status read failed: {error}"))?;
        if size == 0 {
            return Ok(latest);
        }
        latest = parse_status(&packet[..size], product_id).or(latest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_packet(effect: u8) -> Vec<u8> {
        let mut packet = vec![0; PACKET];
        packet[..4].copy_from_slice(&MAGIC);
        packet[4] = CMD_STATUS;
        packet[5] = 0x5A;
        packet[7] = 1;
        packet[13] = 0x14;
        packet[14] = effect;
        packet[15] = 7;
        packet[16] = 4;
        packet[17] = 2;
        packet[18] = 1;
        packet
    }

    fn lighting(effect: u8, brightness: u8, speed: u8, direction: u8, colour: u8) -> Change {
        Change::Lighting {
            effect,
            brightness,
            speed,
            direction,
            colour,
        }
    }

    #[test]
    fn status_request_is_only_the_status_command() {
        let request = status_request();
        assert_eq!(&request[..6], &[0x00, 0xAA, 0x55, 0xCC, 0x33, 0x0E]);
        assert!(request[6..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn lighting_matches_the_confirmed_commands() {
        // Constant, confirmed on the keyboard.
        let request = change_request(lighting(7, 5, 3, 0, 0)).unwrap();
        assert_eq!(
            &request[..11],
            &[0x00, 0xAA, 0x55, 0xCC, 0x33, 0x07, 0x06, 0x04, 0x02, 0x00, 0x00]
        );
        // The saved state the audio test restored.
        let request = change_request(lighting(9, 5, 3, 1, 7)).unwrap();
        assert_eq!(&request[5..11], &[0x07, 0x08, 0x04, 0x02, 0x01, 0x07]);
        assert!(request[11..].iter().all(|&byte| byte == 0));
        assert_eq!(request.len(), 65);
    }

    #[test]
    fn lights_off_request() {
        assert_eq!(
            &change_request(Change::LightsOff).unwrap()[5..8],
            &[0x05, 0x01, 0x00]
        );
    }

    #[test]
    fn out_of_range_lighting_is_refused() {
        for change in [
            lighting(0, 5, 3, 0, 0),
            lighting(21, 5, 3, 0, 0),
            lighting(23, 5, 3, 0, 0),
            lighting(171, 5, 3, 0, 0),
            lighting(7, 0, 3, 0, 0),
            lighting(7, 6, 3, 0, 0),
            lighting(7, 5, 0, 0, 0),
            lighting(7, 5, 6, 0, 0),
            lighting(7, 5, 3, 2, 0),
            lighting(7, 5, 3, 0, 8),
        ] {
            assert!(change_request(change).is_err(), "{change:?}");
        }
    }

    #[test]
    fn only_the_three_allowed_commands_can_be_built() {
        for request in [
            status_request(),
            change_request(Change::LightsOff).unwrap(),
            change_request(lighting(1, 1, 1, 0, 0)).unwrap(),
            music_mode_request(),
        ] {
            assert!([CMD_STATUS, CMD_LIGHTS_OFF, CMD_EFFECT].contains(&request[5]));
        }
    }

    #[test]
    fn music_mode_is_effect_aa() {
        assert_eq!(
            &music_mode_request()[..7],
            &[0x00, 0xAA, 0x55, 0xCC, 0x33, 0x07, 0xAA]
        );
    }

    #[test]
    fn frames_split_into_the_six_pages() {
        let mut frame = [0; FRAME_BYTES];
        for (index, byte) in frame.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        let requests: Vec<_> = frame_requests(&frame).collect();
        assert_eq!(requests.len(), 6);
        for (page, request) in requests.iter().enumerate() {
            assert_eq!(
                &request[..6],
                &[0x00, 0xAA, 0x55, 0xCC, 0x33, 0xF0 + page as u8]
            );
            assert_eq!(&request[6..60], &frame[page * 54..(page + 1) * 54]);
            assert!(request[60..].iter().all(|&byte| byte == 0));
        }
    }

    #[test]
    fn only_normal_effects_can_be_restored() {
        let status = parse_status(&status_packet(9), PID_USB).unwrap();
        assert!(matches!(
            restore_change(&status),
            Ok(Change::Lighting {
                effect: 10,
                brightness: 5,
                speed: 3,
                direction: 1,
                colour: 7
            })
        ));
        for effect in [EFFECT_CUSTOM, EFFECT_MUSIC] {
            assert!(
                restore_change(&parse_status(&status_packet(effect), PID_USB).unwrap()).is_err()
            );
        }
    }

    #[test]
    fn next_effect_follows_the_list_and_wraps() {
        assert!(matches!(
            lighting(14, 5, 3, 1, 7).with_next_effect(),
            Change::Lighting {
                effect: 15,
                brightness: 5,
                speed: 3,
                direction: 1,
                colour: 7
            }
        ));
        assert!(matches!(
            lighting(20, 5, 3, 1, 7).with_next_effect(),
            Change::Lighting { effect: 1, .. }
        ));
        assert!(change_request(lighting(20, 5, 3, 1, 7).with_next_effect()).is_ok());
    }

    #[test]
    fn parses_status() {
        let status = parse_status(&status_packet(6), PID_USB).unwrap();
        assert_eq!(status.connection, "USB");
        assert_eq!(status.battery, 90);
        assert_eq!(status.firmware, "0.0.14");
        assert_eq!(status.effect, "Constant");
        assert_eq!(status.effect_number, Some(7));
        assert_eq!(
            (
                status.brightness,
                status.speed,
                status.direction,
                status.colour
            ),
            (5, 3, 1, 7)
        );
    }

    #[test]
    fn special_effects_have_no_number() {
        let music = parse_status(&status_packet(EFFECT_MUSIC), PID_RECEIVER).unwrap();
        assert_eq!(
            (music.effect.as_str(), music.effect_number),
            ("Music Rhythm", None)
        );
        let custom = parse_status(&status_packet(EFFECT_CUSTOM), PID_USB).unwrap();
        assert_eq!(
            (custom.effect.as_str(), custom.effect_number),
            ("Custom", None)
        );
        assert_eq!(
            parse_status(&status_packet(8), PID_USB).unwrap().effect,
            "Cross Flow"
        );
        assert_eq!(
            parse_status(&status_packet(0xFF), PID_USB).unwrap().effect,
            "Unknown (255)"
        );
    }

    #[test]
    fn ignores_other_packets() {
        let mut other = status_packet(6);
        other[4] = 0x12;
        assert_eq!(parse_status(&other, PID_USB), None);
        assert_eq!(parse_status(&[0; 4], PID_USB), None);
    }

    #[test]
    fn checks_every_lighting_value() {
        let status = parse_status(&status_packet(6), PID_USB).unwrap();
        assert!(matches_change(&status, lighting(7, 5, 3, 1, 7)));
        assert!(matches_change(&status, Change::LightsOff));
        for change in [
            lighting(8, 5, 3, 1, 7),
            lighting(7, 4, 3, 1, 7),
            lighting(7, 5, 2, 1, 7),
            lighting(7, 5, 3, 0, 7),
            lighting(7, 5, 3, 1, 6),
        ] {
            assert!(!matches_change(&status, change), "{change:?}");
        }
    }

    #[test]
    fn change_requests_parse_from_the_window() {
        let change: Change = serde_json::from_str(
            r#"{"kind":"lighting","effect":7,"brightness":5,"speed":3,"direction":0,"colour":0}"#,
        )
        .unwrap();
        assert!(matches!(change, Change::Lighting { effect: 7, .. }));
        assert!(serde_json::from_str::<Change>(r#"{"kind":"factoryReset"}"#).is_err());
    }
}
