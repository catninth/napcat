//! PlayStation kontrollerek (DualSense, DualSense Edge, DualShock 4) USB-n és Bluetooth-on.
//!
//! Az akkuszint a kontroller saját bejövő reportjában van; a bájtok helye a Linux kernel
//! hid-playstation (DualSense) és hid-sony (DualShock 4) drivere szerint. Bluetooth-on a
//! kontroller alapból rövid reportot küld akku nélkül; a kalibrációs feature report kiolvasása
//! kapcsolja át a teljes reportra (ugyanígy, ahogy a Linux driver és a Steam teszi).

use std::collections::HashMap;
use std::time::Duration;

use super::hid::{self, Device, HidInterface};
use super::Provider;
use crate::model::{ChargeState, DeviceKind, DeviceState};

const SONY: u16 = 0x054C;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Family {
    DualSense,
    DualShock4,
}

fn model(pid: u16) -> Option<(Family, &'static str)> {
    Some(match pid {
        0x0CE6 => (Family::DualSense, "DualSense"),
        0x0DF2 => (Family::DualSense, "DualSense Edge"),
        0x05C4 | 0x09CC => (Family::DualShock4, "DualShock 4"),
        0x0BA0 => (Family::DualShock4, "DualShock 4 (vevő)"),
        _ => return None,
    })
}

/// Állapotbájt -> (szint %, töltés). DualSense: alsó 4 bit 0-10 szint, felső 4 bit
/// 0 = merül, 1 = tölt, 2 = feltöltve. A szint 10%-os lépésekben jön, a közepét adjuk.
fn dualsense_status(s: u8) -> Option<(u8, ChargeState)> {
    let level = ((s & 0x0F) * 10 + 5).min(100);
    match s >> 4 {
        0 => Some((level, ChargeState::Discharging)),
        1 => Some((level, ChargeState::Charging)),
        2 => Some((100, ChargeState::Full)),
        _ => None, // hőmérséklet- vagy töltési hiba
    }
}

/// DualShock 4: alsó 4 bit szint (kábelen 0-11, 11 = teli), 0x10 bit = kábel csatlakoztatva.
fn dualshock4_status(s: u8) -> (u8, ChargeState) {
    let cap = s & 0x0F;
    let cable = s & 0x10 != 0;
    match (cable, cap) {
        (true, 0..=9) => (cap * 10 + 5, ChargeState::Charging),
        (true, _) => (100, ChargeState::Full),
        (false, 0..=9) => (cap * 10 + 5, ChargeState::Discharging),
        (false, _) => (100, ChargeState::Discharging),
    }
}

/// Akkuállapot egy bejövő reportból (az első bájt a report ID).
fn parse(family: Family, bluetooth: bool, report: &[u8]) -> Option<(u8, ChargeState)> {
    let (id, offset) = match (family, bluetooth) {
        (Family::DualSense, false) => (0x01, 53),
        (Family::DualSense, true) => (0x31, 54),
        (Family::DualShock4, false) => (0x01, 30),
        (Family::DualShock4, true) => (0x11, 32),
    };
    if report.first() != Some(&id) {
        return None;
    }
    let s = *report.get(offset)?;
    match family {
        Family::DualSense => dualsense_status(s),
        Family::DualShock4 => Some(dualshock4_status(s)),
    }
}

/// Egy kontroller kiolvasása: néhány report, amíg egy akkut tartalmazó nem jön.
fn read(i: &HidInterface, family: Family) -> Option<(u8, ChargeState)> {
    let dev = Device::open(&i.path)?;
    let bluetooth = i.is_bluetooth();
    if bluetooth {
        // Kalibrációs feature report: átkapcsol a teljes (akkut is tartalmazó) reportra.
        let _ = match family {
            Family::DualSense => dev.get_feature(0x05, 41),
            Family::DualShock4 => dev.get_feature(0x02, 37),
        };
    }
    (0..20).find_map(|_| parse(family, bluetooth, &dev.read(i.input_len as usize, 250)?))
}

#[derive(Default)]
pub struct PlayStation {
    /// Az ebben a munkamenetben látott kontrollerek (id -> név); eltűnésük után "nincs csatlakoztatva".
    seen: HashMap<String, String>,
    connected: usize,
}

impl Provider for PlayStation {
    fn scan(&mut self) -> Vec<DeviceState> {
        let mut out: Vec<DeviceState> = Vec::new();
        // A gamepad kollekció (Generic Desktop / Game Pad) olvasható; egy kontroller egyszer.
        for i in hid::interfaces().iter().filter(|i| i.vid == SONY && i.usage_page == 0x01 && i.usage == 0x05) {
            let Some((family, name)) = model(i.pid) else { continue };
            let id = format!("sony-{:04x}-{}", i.pid, if i.serial.is_empty() { "0" } else { &i.serial });
            if out.iter().any(|d| d.id == id) {
                continue;
            }
            let (level, charge) = match read(i, family) {
                Some((l, c)) => (Some(l), c),
                None => (None, ChargeState::Unknown),
            };
            self.seen.insert(id.clone(), name.to_string());
            out.push(DeviceState { id, name: name.into(), kind: DeviceKind::Controller, level, level_approx: false, charge, connected: true });
        }
        self.connected = out.len();
        for (id, name) in &self.seen {
            if !out.iter().any(|d| &d.id == id) {
                out.push(DeviceState::disconnected(id, name, DeviceKind::Controller));
            }
        }
        out
    }

    fn poll_interval(&self) -> Option<Duration> {
        (self.connected > 0).then_some(Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A most csatlakoztatott kontrollerek (`cargo test playstation::tests::scan_present -- --ignored --nocapture`).
    #[test]
    #[ignore = "a csatlakoztatott hardvertől függ"]
    fn scan_present() {
        let t = std::time::Instant::now();
        let all = hid::interfaces();
        println!("interfaces(): {} db, {:?}", all.len(), t.elapsed());
        let t = std::time::Instant::now();
        let _ = PlayStation::default().scan();
        println!("scan(): {:?}", t.elapsed());
        for i in all.iter().filter(|i| i.vid == SONY && i.usage_page == 0x01 && i.usage == 0x05) {
            let Some((family, name)) = model(i.pid) else { continue };
            let bt = i.is_bluetooth();
            let dev = Device::open(&i.path).expect("megnyitás");
            let offset = match (family, bt) {
                (Family::DualSense, false) => 53,
                (Family::DualSense, true) => 54,
                (Family::DualShock4, false) => 30,
                (Family::DualShock4, true) => 32,
            };
            let report = dev.read(i.input_len as usize, 500).expect("report");
            println!("{name} bt={bt} len={} id=0x{:02X} akku bájt=0x{:02X}", report.len(), report[0], report[offset]);
        }
        for d in PlayStation::default().scan() {
            println!("{}", d.tooltip());
        }
    }

    #[test]
    fn dualsense_usb_and_bluetooth() {
        let mut usb = vec![0u8; 64];
        usb[0] = 0x01;
        usb[53] = 0x07; // 7 -> 75%, merül
        assert_eq!(parse(Family::DualSense, false, &usb), Some((75, ChargeState::Discharging)));
        usb[53] = 0x13; // tölt, 35%
        assert_eq!(parse(Family::DualSense, false, &usb), Some((35, ChargeState::Charging)));
        usb[53] = 0x2A; // feltöltve
        assert_eq!(parse(Family::DualSense, false, &usb), Some((100, ChargeState::Full)));
        usb[53] = 0xF0; // töltési hiba
        assert_eq!(parse(Family::DualSense, false, &usb), None);

        let mut bt = vec![0u8; 78];
        bt[0] = 0x31;
        bt[54] = 0x0A; // 10 -> 100%
        assert_eq!(parse(Family::DualSense, true, &bt), Some((100, ChargeState::Discharging)));
        // Rövid (akku nélküli) Bluetooth report.
        assert_eq!(parse(Family::DualSense, true, &[0x01, 0, 0, 0]), None);
    }

    #[test]
    fn dualshock4_usb_and_bluetooth() {
        let mut usb = vec![0u8; 64];
        usb[0] = 0x01;
        usb[30] = 0x04; // 45%, nincs kábel
        assert_eq!(parse(Family::DualShock4, false, &usb), Some((45, ChargeState::Discharging)));
        usb[30] = 0x16; // kábelen, 65%
        assert_eq!(parse(Family::DualShock4, false, &usb), Some((65, ChargeState::Charging)));
        usb[30] = 0x1B; // kábelen, teli
        assert_eq!(parse(Family::DualShock4, false, &usb), Some((100, ChargeState::Full)));

        let mut bt = vec![0u8; 547];
        bt[0] = 0x11;
        bt[32] = 0x02;
        assert_eq!(parse(Family::DualShock4, true, &bt), Some((25, ChargeState::Discharging)));
    }
}
