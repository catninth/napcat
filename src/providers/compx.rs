//! Compx vevős egerek (VGN, ATK, VXE): akkuszint a vevőn keresztül, az ATK HUB webes driver
//! protokollja szerint.
//!
//! Keret: 16 bájt a 8-as reportban (0xFF02/0x0002 kollekció): [parancs, állapot, cím (2), hossz,
//! adat (10), ellenőrzőösszeg], az összeg 0x55 - (8 + az első 15 bájt összege). Csak olvasó
//! parancsokat küldünk: egér online?, kézfogás (ezt a driver is minden lekérdezés előtt küldi),
//! akkuszint. Alvó egér nem válaszol; ilyenkor az utolsó ismert szint marad.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::hid::{self, Device, HidInterface};
use super::Provider;
use crate::model::{ChargeState, DeviceKind, DeviceState};

const COMPX: u16 = 0x3554;
const REPORT: u8 = 0x08;
const HANDSHAKE: u8 = 0x01;
const GET_MOUSE_ONLINE: u8 = 0x03;
const GET_BATTERY_LEVEL: u8 = 0x04;

fn frame(cmd: u8, data: &[u8]) -> [u8; 17] {
    let mut f = [0u8; 17];
    f[0] = REPORT;
    f[1] = cmd;
    f[5] = data.len() as u8;
    f[6..6 + data.len()].copy_from_slice(data);
    let sum: u32 = REPORT as u32 + f[1..16].iter().map(|&b| b as u32).sum::<u32>();
    f[16] = 0x55u8.wrapping_sub((sum & 0xFF) as u8);
    f
}

/// Kérés és a hozzá tartozó válasz (az első adatbájt a parancs), legfeljebb 3 próbálkozással.
fn transfer(dev: &Device, cmd: u8, data: &[u8]) -> Option<Vec<u8>> {
    let req = frame(cmd, data);
    for _ in 0..3 {
        if !dev.write(&req, 300) {
            return None;
        }
        let deadline = Instant::now() + Duration::from_millis(300);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Some(r) = dev.read(17, left.as_millis().max(1) as u32) else { break };
            if r.len() >= 10 && r[0] == REPORT && r[1] == cmd {
                return Some(r);
            }
        }
    }
    None
}

/// Az akkuválasz: szint % és töltés (az adatrész 0. és 1. bájtja).
fn parse_battery(r: &[u8]) -> Option<(u8, ChargeState)> {
    if r.len() < 8 || r[1] != GET_BATTERY_LEVEL || r[2] != 0 {
        return None;
    }
    let charge = if r[7] != 0 { ChargeState::Charging } else { ChargeState::Discharging };
    Some((r[6].min(100), charge))
}

/// None: az egér alszik vagy nem érhető el; Some: a friss akkuállapot.
fn read(i: &HidInterface) -> Option<(u8, ChargeState)> {
    use crate::worker::log;
    let Some(dev) = Device::open(&i.path) else {
        log("compx: a vevő nem nyitható meg");
        return None;
    };
    let Some(online) = transfer(&dev, GET_MOUSE_ONLINE, &[]) else {
        log("compx: a vevő nem válaszol");
        return None;
    };
    if online[6] == 0 {
        log("compx: az egér alszik");
        return None;
    }
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos()).to_le_bytes();
    transfer(&dev, HANDSHAKE, &[seed[0], seed[1], seed[2], seed[3], 0, 0, 0, 0])?;
    let battery = transfer(&dev, GET_BATTERY_LEVEL, &[]).and_then(|r| parse_battery(&r));
    log(&format!("compx: akku {battery:?}"));
    battery
}

/// "VGN Dragonfly 4K Receiver" -> "VGN Dragonfly".
fn device_name(product: &str) -> String {
    let name = product.trim();
    let name = ["4K Receiver", "8K Receiver", "2K Receiver", "1K Receiver", "Receiver", "Dongle"]
        .iter()
        .find_map(|s| name.strip_suffix(s))
        .unwrap_or(name)
        .trim();
    if name.is_empty() { "Compx egér".into() } else { name.into() }
}

#[derive(Default)]
pub struct Compx {
    /// Eszközönként: név és az utolsó ismert szint (alvás közben ez marad).
    seen: HashMap<String, (String, Option<(u8, ChargeState)>)>,
    connected: usize,
    any_unknown: bool,
}

impl Provider for Compx {
    fn scan(&mut self) -> Vec<DeviceState> {
        let mut out: Vec<DeviceState> = Vec::new();
        let receivers = hid::interfaces()
            .into_iter()
            .filter(|i| i.vid == COMPX && i.usage_page == 0xFF02 && i.usage == 0x0002 && i.input_len == 17);
        for i in receivers {
            let id = format!("compx-{:04x}-{}", i.pid, if i.serial.is_empty() { "0" } else { &i.serial });
            if out.iter().any(|d| d.id == id) {
                continue;
            }
            let entry = self.seen.entry(id.clone()).or_insert_with(|| (device_name(&i.product), None));
            if let Some(fresh) = read(&i) {
                entry.1 = Some(fresh);
            }
            let (level, charge) = match entry.1 {
                Some((l, c)) => (Some(l), c),
                None => (None, ChargeState::Unknown),
            };
            out.push(DeviceState { level, charge, connected: true, ..DeviceState::disconnected(&id, &entry.0, DeviceKind::Mouse) });
        }
        self.connected = out.len();
        self.any_unknown = out.iter().any(|d| d.level.is_none());
        for (id, (name, _)) in &self.seen {
            if !out.iter().any(|d| &d.id == id) {
                out.push(DeviceState::disconnected(id, name, DeviceKind::Mouse));
            }
        }
        out
    }

    /// Amíg nincs ismert szint (pl. az egér aludt), percenként próbálunk; utána 5 percenként.
    fn poll_interval(&self) -> Option<Duration> {
        (self.connected > 0).then_some(Duration::from_secs(if self.any_unknown { 60 } else { 300 }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_and_battery() {
        // A vevőtől rögzített keretek (VGN Dragonfly 4K Receiver).
        assert_eq!(frame(GET_BATTERY_LEVEL, &[])[16], 0x49);
        assert_eq!(frame(GET_MOUSE_ONLINE, &[])[16], 0x4A);
        assert_eq!(frame(HANDSHAKE, &[0xA0, 0x18, 0xAB, 0x28, 0, 0, 0, 0])[16], 0xB9);
        let resp = [0x08, 0x04, 0x00, 0x00, 0x00, 0x02, 0x32, 0x00, 0x0F, 0x1B, 0, 0, 0, 0, 0, 0, 0xEB];
        assert_eq!(parse_battery(&resp), Some((50, ChargeState::Discharging)));
        let charging = [0x08, 0x04, 0x00, 0x00, 0x00, 0x02, 0x48, 0x01, 0x10, 0x20, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(parse_battery(&charging), Some((72, ChargeState::Charging)));
        assert_eq!(device_name("VGN Dragonfly 4K Receiver"), "VGN Dragonfly");
    }

    /// `cargo test compx_present -- --ignored --nocapture` (mozgasd közben az egeret).
    #[test]
    #[ignore = "a csatlakoztatott hardvertől függ"]
    fn compx_present() {
        let mut p = Compx::default();
        for _ in 0..30 {
            let devices = p.scan();
            if devices.iter().all(|d| d.level.is_some()) {
                for d in devices {
                    println!("{}", d.tooltip());
                }
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        println!("30 mp alatt nem jött szint (az egér aludt)");
    }
}
