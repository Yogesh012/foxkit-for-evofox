//! Banshee 2 settings channel.
//!
//! Requests can only be built from the `Read` and `Write` lists, so only these settings can
//! change, and the bootloader command can't be sent.

use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hidapi::{DeviceInfo, HidDevice};
use serde::{Deserialize, Serialize};

use crate::dpi_table;

const VID: u16 = 0xB6A4;
const USAGE_PAGE: u16 = 0xFF01;
const USAGE: u16 = 0x0001;
const REPORT: u8 = 6;
const PACKET: usize = 40;
const HEADER: usize = 8;
const REPLY_WAIT: Duration = Duration::from_millis(130);
const WRITE_WAIT: Duration = Duration::from_millis(50);
const DPI_LEVELS: usize = 8;
const DPI_TABLE_BYTES: usize = 20;
const DPI_VALUES_START: usize = 2;
const CHIP_WITH_KNOWN_TABLE: u8 = 8;
const DEBOUNCE_MIN: u8 = 1;
const DEBOUNCE_MAX: u8 = 20;
const BATTERY_CHARGING_BIT: u8 = 0x80;
const BATTERY_MAX_PERCENT: u8 = 100;
// Listed under this name when connected over Bluetooth, with a different vendor ID.
pub const BLUETOOTH_NAME: &str = "Banshee";

type DpiTable = [u8; DPI_TABLE_BYTES];

#[derive(Clone, Copy)]
enum Read {
    Connection,
    ReportRate,
    DpiTable,
    ShortDebounce,
    Firmware,
}

impl Read {
    /// Command with its read bit set, sub-ID, length.
    fn spec(self) -> (u8, u8, u8) {
        match self {
            Read::Connection => (0x81, 0x00, 3),
            Read::ReportRate => (0x83, 0x00, 1),
            Read::DpiTable => (0x86, 0x00, DPI_TABLE_BYTES as u8),
            Read::ShortDebounce => (0x8A, 0x02, 1),
            Read::Firmware => (0xF0, 0x01, 4),
        }
    }
}

#[derive(Clone, Copy)]
enum Write {
    ReportRate(u8),
    DpiTable(DpiTable),
    ShortDebounce(u8),
    /// Sent after every setting, with two new random bytes.
    ChangeMarker([u8; 2]),
}

impl Write {
    fn packet(&self, seq: u8) -> [u8; PACKET + 1] {
        let (cmd, sub, data): (u8, u8, &[u8]) = match self {
            Write::ReportRate(code) => (0x03, 0x00, std::slice::from_ref(code)),
            Write::DpiTable(table) => (0x06, 0x00, table),
            Write::ShortDebounce(value) => (0x0A, 0x02, std::slice::from_ref(value)),
            Write::ChangeMarker(random) => (0x02, 0x01, random),
        };
        let mut packet = [0; PACKET + 1];
        packet[0] = REPORT;
        packet[3] = cmd;
        packet[4] = seq;
        packet[5] = sub;
        packet[8] = data.len() as u8;
        packet[HEADER + 1..HEADER + 1 + data.len()].copy_from_slice(data);
        packet
    }
}

/// A change asked for in the window.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Change {
    ReportRate { hz: u16 },
    ActiveLevel { level: u8 },
    LevelDpi { level: u8, dpi: u16 },
    ShortDebounce { value: u8 },
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Level {
    pub number: u8,
    pub value: u16,
    /// None when the DPI table doesn't cover this mouse's chip or this value.
    pub dpi: Option<u16>,
    pub active: bool,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub chip: u8,
    pub battery_percent: u8,
    pub charging: bool,
    pub report_rate_hz: Option<u16>,
    pub levels: Vec<Level>,
    pub short_debounce: u8,
    pub firmware: String,
}

pub fn is_settings_channel(info: &DeviceInfo) -> bool {
    info.vendor_id() == VID && info.usage_page() == USAGE_PAGE && info.usage() == USAGE
}

pub fn is_this_device(info: &DeviceInfo) -> bool {
    info.vendor_id() == VID
        || info
            .product_string()
            .is_some_and(|name| name.contains(BLUETOOTH_NAME))
}

fn read_request(read: Read, seq: u8) -> [u8; PACKET + 1] {
    let (cmd, sub, length) = read.spec();
    let mut request = [0; PACKET + 1];
    request[0] = REPORT;
    request[3] = cmd;
    request[4] = seq;
    request[5] = sub;
    request[8] = length;
    request
}

/// Returns the data if the reply echoes the request.
fn parse_reply(read: Read, seq: u8, reply: &[u8]) -> Option<Vec<u8>> {
    let request = read_request(read, seq);
    let end = HEADER + 1 + usize::from(read.spec().2);
    if reply.len() < end || reply[..=HEADER] != request[..=HEADER] {
        return None;
    }
    Some(reply[HEADER + 1..end].to_vec())
}

fn parse_report_rate(code: u8) -> Option<u16> {
    match code {
        0x01 => Some(1000),
        0x02 => Some(500),
        0x04 => Some(250),
        0x08 => Some(125),
        _ => None,
    }
}

fn report_rate_code(hz: u16) -> Option<u8> {
    [0x01, 0x02, 0x04, 0x08]
        .into_iter()
        .find(|&code| parse_report_rate(code) == Some(hz))
}

fn parse_battery(raw: u8) -> (u8, bool) {
    (
        (raw & !BATTERY_CHARGING_BIT).min(BATTERY_MAX_PERCENT),
        raw & BATTERY_CHARGING_BIT != 0,
    )
}

fn is_enabled(table: &DpiTable, level: u8) -> bool {
    (1..=DPI_LEVELS as u8).contains(&level) && table[0] >> (level - 1) & 1 == 1
}

fn parse_levels(table: &DpiTable, chip: u8) -> Vec<Level> {
    let active = table[1];
    // The active level counts from 1.
    (1..=DPI_LEVELS as u8)
        .filter(|&number| is_enabled(table, number))
        .map(|number| {
            let start = DPI_VALUES_START + 2 * usize::from(number - 1);
            let value = u16::from_le_bytes([table[start], table[start + 1]]);
            Level {
                number,
                value,
                dpi: if chip == CHIP_WITH_KNOWN_TABLE {
                    dpi_table::chip_8_dpi(value)
                } else {
                    None
                },
                active: number == active,
            }
        })
        .collect()
}

/// Returns the tables to write in order. The last one is what the mouse should end up with.
fn plan_table(table: &DpiTable, chip: u8, change: Change) -> Result<Vec<DpiTable>, String> {
    let check_level = |level| {
        if is_enabled(table, level) {
            Ok(())
        } else {
            Err(format!("Level {level} isn't enabled on this mouse."))
        }
    };
    match change {
        Change::ActiveLevel { level } => {
            check_level(level)?;
            let mut changed = *table;
            changed[1] = level;
            Ok(vec![changed])
        }
        Change::LevelDpi { level, dpi } => {
            check_level(level)?;
            if chip != CHIP_WITH_KNOWN_TABLE {
                return Err(format!("DPI values aren't known for chip {chip}."));
            }
            let value = dpi_table::chip_8_value(dpi)
                .ok_or_else(|| format!("{dpi} DPI isn't available on this mouse."))?;
            let mut changed = *table;
            changed[DPI_VALUES_START + 2 * usize::from(level - 1)..][..2]
                .copy_from_slice(&value.to_le_bytes());
            if level != table[1] {
                return Ok(vec![changed]);
            }
            // The sensor only takes a new value when the mouse switches to that level.
            let other = (1..=DPI_LEVELS as u8)
                .find(|&candidate| candidate != level && is_enabled(table, candidate))
                .ok_or("This mouse needs a second enabled level to change the active one.")?;
            let mut away = changed;
            away[1] = other;
            Ok(vec![changed, away, changed])
        }
        Change::ReportRate { .. } | Change::ShortDebounce { .. } => Ok(Vec::new()),
    }
}

struct Session<'a> {
    device: &'a HidDevice,
    seq: u8,
}

impl Session<'_> {
    fn next_seq(&mut self) -> u8 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    fn read(&mut self, read: Read) -> Result<Vec<u8>, String> {
        let seq = self.next_seq();
        self.device
            .send_feature_report(&read_request(read, seq))
            .map_err(|error| format!("Mouse request failed: {error}"))?;
        sleep(REPLY_WAIT);
        let mut reply = [0; PACKET + 1];
        reply[0] = REPORT;
        let size = self
            .device
            .get_feature_report(&mut reply)
            .map_err(|error| format!("Mouse reply failed: {error}"))?;
        parse_reply(read, seq, &reply[..size])
            .ok_or_else(|| "The mouse's reply didn't match the request.".to_string())
    }

    fn write_setting(&mut self, write: Write) -> Result<(), String> {
        for step in [write, Write::ChangeMarker(random_bytes())] {
            let seq = self.next_seq();
            self.device
                .send_feature_report(&step.packet(seq))
                .map_err(|error| format!("Mouse write failed: {error}"))?;
            sleep(WRITE_WAIT);
        }
        Ok(())
    }

    fn read_table(&mut self) -> Result<DpiTable, String> {
        DpiTable::try_from(self.read(Read::DpiTable)?)
            .map_err(|_| "The mouse's DPI table was the wrong size.".to_string())
    }

    fn read_status(&mut self) -> Result<Status, String> {
        let connection = self.read(Read::Connection)?;
        let rate = self.read(Read::ReportRate)?;
        let table = self.read_table()?;
        let debounce = self.read(Read::ShortDebounce)?;
        let firmware = self.read(Read::Firmware)?;
        let (battery_percent, charging) = parse_battery(connection[1]);
        Ok(Status {
            chip: connection[2],
            battery_percent,
            charging,
            report_rate_hz: parse_report_rate(rate[0]),
            levels: parse_levels(&table, connection[2]),
            short_debounce: debounce[0],
            firmware: String::from_utf8_lossy(&firmware)
                .trim_end_matches(['\0', '.'])
                .to_string(),
        })
    }
}

fn random_bytes() -> [u8; 2] {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    let mixed = nanos.wrapping_mul(2_654_435_761);
    [(mixed >> 24) as u8, (mixed >> 16) as u8]
}

pub fn read_status(device: &HidDevice, _product_id: u16) -> Result<Status, String> {
    Session { device, seq: 0 }.read_status()
}

/// Writes one change, checks the mouse stored it, and returns the new status.
pub fn apply(device: &HidDevice, change: Change) -> Result<Status, String> {
    let mut session = Session { device, seq: 0 };
    match change {
        Change::ReportRate { hz } => {
            let code = report_rate_code(hz)
                .ok_or_else(|| format!("{hz} Hz isn't a report rate this mouse has."))?;
            session.write_setting(Write::ReportRate(code))?;
            if session.read(Read::ReportRate)? != [code] {
                return Err("The mouse didn't store the new report rate.".to_string());
            }
        }
        Change::ShortDebounce { value } => {
            if !(DEBOUNCE_MIN..=DEBOUNCE_MAX).contains(&value) {
                return Err(format!(
                    "Short debounce has to be from {DEBOUNCE_MIN} to {DEBOUNCE_MAX}."
                ));
            }
            session.write_setting(Write::ShortDebounce(value))?;
            if session.read(Read::ShortDebounce)? != [value] {
                return Err("The mouse didn't store the new short debounce.".to_string());
            }
        }
        Change::ActiveLevel { .. } | Change::LevelDpi { .. } => {
            let chip = session.read(Read::Connection)?[2];
            let table = session.read_table()?;
            let plan = plan_table(&table, chip, change)?;
            for step in &plan {
                session.write_setting(Write::DpiTable(*step))?;
            }
            if plan
                .last()
                .is_some_and(|last| session.read_table().ok().as_ref() != Some(last))
            {
                return Err("The mouse didn't store the new DPI table.".to_string());
            }
        }
    }
    session.read_status()
}

#[cfg(test)]
mod tests {
    use super::*;

    const YOUR_TABLE: DpiTable = [
        0x1F, 0x02, 0x06, 0x00, 0x12, 0x00, 0x26, 0x00, 0x4C, 0x00, 0x8F, 0x00, 0x8F, 0x00, 0x8F,
        0x00, 0x8F, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn read_request_layout() {
        let request = read_request(Read::DpiTable, 3);
        assert_eq!(&request[..9], &[6, 0, 0, 0x86, 3, 0, 0, 0, 20]);
        assert_eq!(request.len(), 41);
        assert!(request[9..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn every_request_is_a_read_and_never_the_bootloader() {
        for read in [
            Read::Connection,
            Read::ReportRate,
            Read::DpiTable,
            Read::ShortDebounce,
            Read::Firmware,
        ] {
            let (cmd, _, _) = read.spec();
            assert_ne!(cmd & 0x80, 0);
            assert_ne!(cmd & 0x7F, 0x7F);
        }
    }

    #[test]
    fn write_packet_layout() {
        assert_eq!(
            &Write::ReportRate(0x02).packet(4)[..10],
            &[6, 0, 0, 0x03, 4, 0, 0, 0, 1, 0x02]
        );
        assert_eq!(
            &Write::ShortDebounce(8).packet(5)[..10],
            &[6, 0, 0, 0x0A, 5, 2, 0, 0, 1, 8]
        );
        assert_eq!(
            &Write::ChangeMarker([0x22, 0xB4]).packet(6)[..11],
            &[6, 0, 0, 0x02, 6, 1, 0, 0, 2, 0x22, 0xB4]
        );
        let table = Write::DpiTable(YOUR_TABLE).packet(7);
        assert_eq!(&table[..9], &[6, 0, 0, 0x06, 7, 0, 0, 0, 20]);
        assert_eq!(table[9..29], YOUR_TABLE);
        assert_eq!(table.len(), 41);
    }

    #[test]
    fn no_write_is_a_read_or_the_bootloader() {
        for write in [
            Write::ReportRate(1),
            Write::DpiTable(YOUR_TABLE),
            Write::ShortDebounce(8),
            Write::ChangeMarker([0, 0]),
        ] {
            let cmd = write.packet(1)[3];
            assert_eq!(cmd & 0x80, 0);
            assert_ne!(cmd, 0x7F);
        }
    }

    #[test]
    fn reply_must_echo_the_request() {
        let mut reply = read_request(Read::ReportRate, 2).to_vec();
        reply[9] = 0x01;
        assert_eq!(parse_reply(Read::ReportRate, 2, &reply), Some(vec![0x01]));
        assert_eq!(parse_reply(Read::ReportRate, 3, &reply), None);
        assert_eq!(parse_reply(Read::ReportRate, 2, &reply[..9]), None);
    }

    #[test]
    fn active_level_counts_from_one() {
        let levels = parse_levels(&YOUR_TABLE, 8);
        assert_eq!(levels.len(), 5);
        let active: Vec<_> = levels.iter().filter(|level| level.active).collect();
        assert_eq!(active.len(), 1);
        assert_eq!(
            (active[0].number, active[0].value, active[0].dpi),
            (2, 18, Some(1600))
        );
        assert_eq!(levels[2].dpi, Some(3200));
    }

    #[test]
    fn other_chips_get_no_dpi_label() {
        assert!(parse_levels(&YOUR_TABLE, 7)
            .iter()
            .all(|level| level.dpi.is_none()));
    }

    #[test]
    fn report_rates() {
        assert_eq!(parse_report_rate(0x01), Some(1000));
        assert_eq!(parse_report_rate(0x02), Some(500));
        assert_eq!(parse_report_rate(0x03), None);
        assert_eq!(report_rate_code(500), Some(0x02));
        assert_eq!(report_rate_code(333), None);
    }

    #[test]
    fn battery_byte_holds_percent_and_charging() {
        assert_eq!(parse_battery(0xDF), (95, true));
        assert_eq!(parse_battery(0x5F), (95, false));
        assert_eq!(parse_battery(0xFF), (100, true));
        assert_eq!(parse_battery(0x00), (0, false));
    }

    #[test]
    fn switching_level_changes_only_the_active_byte() {
        let plan = plan_table(&YOUR_TABLE, 8, Change::ActiveLevel { level: 3 }).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0][1], 3);
        assert_eq!(plan[0][0], YOUR_TABLE[0]);
        assert_eq!(plan[0][2..], YOUR_TABLE[2..]);
    }

    #[test]
    fn disabled_levels_are_refused() {
        for level in [0, 6, 8, 9] {
            assert!(plan_table(&YOUR_TABLE, 8, Change::ActiveLevel { level }).is_err());
            assert!(plan_table(&YOUR_TABLE, 8, Change::LevelDpi { level, dpi: 800 }).is_err());
        }
    }

    #[test]
    fn editing_another_level_is_one_write() {
        let plan = plan_table(&YOUR_TABLE, 8, Change::LevelDpi { level: 3, dpi: 800 }).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0][6..8], [9, 0]);
        assert_eq!(plan[0][1], 2);
    }

    #[test]
    fn editing_the_active_level_switches_away_and_back() {
        let plan = plan_table(&YOUR_TABLE, 8, Change::LevelDpi { level: 2, dpi: 800 }).unwrap();
        assert_eq!(
            plan.iter().map(|table| table[1]).collect::<Vec<_>>(),
            [2, 1, 2]
        );
        assert!(plan.iter().all(|table| table[4..6] == [9, 0]));
        assert_eq!(plan[0], plan[2]);
    }

    #[test]
    fn high_values_use_both_bytes_correctly() {
        let plan = plan_table(
            &YOUR_TABLE,
            8,
            Change::LevelDpi {
                level: 1,
                dpi: 12000,
            },
        )
        .unwrap();
        assert_eq!(plan[0][2..4], [143, 0]);
    }

    #[test]
    fn unknown_dpi_and_chip_are_refused() {
        assert!(plan_table(&YOUR_TABLE, 8, Change::LevelDpi { level: 3, dpi: 850 }).is_err());
        assert!(plan_table(&YOUR_TABLE, 7, Change::LevelDpi { level: 3, dpi: 800 }).is_err());
    }

    #[test]
    fn change_requests_parse_from_the_window() {
        let change: Change =
            serde_json::from_str(r#"{"kind":"levelDpi","level":3,"dpi":800}"#).unwrap();
        assert!(matches!(change, Change::LevelDpi { level: 3, dpi: 800 }));
        assert!(serde_json::from_str::<Change>(r#"{"kind":"bootloader"}"#).is_err());
    }
}
