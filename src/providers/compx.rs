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
    if name.is_empty() { crate::i18n::t("Compx egér", "Compx mouse").into() } else { name.into() }
}

/// Vevő (dongle), nem pedig kábellel csatlakoztatott egér.
fn is_receiver(product: &str) -> bool {
    let p = product.to_ascii_lowercase();
    p.contains("receiver") || p.contains("dongle")
}

/// Kábeles egér PID -> a vevője PID-je, ahol ellenőriztük, hogy összetartoznak.
/// Kábeles egér PID -> a vevője PID-je és a név, ahol ellenőriztük, hogy összetartoznak.
const CABLE_TO_RECEIVER: &[(u16, u16, &str)] = &[
    (0xF506, 0xF505, "VGN Dragonfly"), // kábelen "VGN F1 MOBA", vevőn "VGN Dragonfly 4K Receiver"
];

/// Egy egér a munkamenetben: név, utolsó ismert szint (alvás közben ez marad), és hogy az kábelről jött-e.
struct Seen {
    name: String,
    last: Option<(u8, ChargeState)>,
    from_cable: bool,
}

#[derive(Default)]
pub struct Compx {
    seen: HashMap<String, Seen>,
    connected: usize,
    any_unknown: bool,
}

impl Compx {
    /// Az egér azonosítója a vevője szerint, így kábelen (töltés közben) is ugyanaz az eszköz marad.
    /// A kábel PID-je a táblázatból, vagy ha nincs benne: a vevőé eggyel kisebb, ha ilyen vevőt már láttunk.
    fn device_id(&self, i: &HidInterface, cable: bool) -> String {
        let serial = if i.serial.is_empty() { "0" } else { &i.serial };
        let receiver_id = |pid: u16| format!("compx-{pid:04x}-{serial}");
        if cable {
            if let Some(&(_, r, _)) = CABLE_TO_RECEIVER.iter().find(|(c, _, _)| *c == i.pid) {
                return receiver_id(r);
            }
            let guess = receiver_id(i.pid.wrapping_sub(1));
            if self.seen.contains_key(&guess) {
                return guess;
            }
        }
        receiver_id(i.pid)
    }
}

impl Provider for Compx {
    fn scan(&mut self) -> Vec<DeviceState> {
        let mut out: Vec<DeviceState> = Vec::new();
        let interfaces = hid::interfaces()
            .into_iter()
            .filter(|i| i.vid == COMPX && i.usage_page == 0xFF02 && i.usage == 0x0002 && i.input_len == 17);
        // Azonosítónként a friss olvasás és a hozzá tartozó VID/PID; a kábel nyer, ha az is válaszol.
        let mut fresh: Vec<(String, Option<(u8, ChargeState)>, HidInterface, bool)> = Vec::new();
        for i in interfaces {
            let cable = !is_receiver(&i.product);
            let id = self.device_id(&i, cable);
            // Kábelen az egér tápot kap: ha épp nem tölt (pl. 95% fölött leáll), az "feltöltve".
            let reading = read(&i).map(|(l, c)| (l, if cable && c == ChargeState::Discharging { ChargeState::Full } else { c }));
            match fresh.iter_mut().find(|f| f.0 == id) {
                Some(f) => {
                    if reading.is_some() && (f.1.is_none() || cable) {
                        *f = (id, reading, i, cable);
                    }
                }
                None => fresh.push((id, reading, i, cable)),
            }
        }
        for (id, reading, i, cable) in fresh {
            let entry = self.seen.entry(id.clone()).or_insert_with(|| {
                let known = CABLE_TO_RECEIVER.iter().find(|(c, r, _)| *c == i.pid || *r == i.pid).map(|k| k.2.to_string());
                Seen { name: known.unwrap_or_else(|| device_name(&i.product)), last: None, from_cable: false }
            });
            match reading {
                Some(fresh) => {
                    entry.last = Some(fresh);
                    entry.from_cable = cable;
                }
                // Kihúzott kábel után, amíg a vevőn alvó egér nem válaszol: a szint marad, a töltés nem.
                None if entry.from_cable && !cable => {
                    if let Some((_, c)) = entry.last.as_mut() {
                        *c = ChargeState::Discharging;
                    }
                    entry.from_cable = false;
                }
                None => {}
            }
            let (level, charge) = match entry.last {
                Some((l, c)) => (Some(l), c),
                None => (None, ChargeState::Unknown),
            };
            out.push(DeviceState {
                level,
                charge,
                connected: true,
                hw: Some((i.vid, i.pid)),
                ..DeviceState::disconnected(&id, &entry.name, DeviceKind::Mouse)
            });
        }
        self.connected = out.len();
        self.any_unknown = out.iter().any(|d| d.level.is_none());
        for (id, Seen { name, .. }) in &self.seen {
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

    #[test]
    fn cable_joins_receiver() {
        let iface = |pid: u16, product: &str| HidInterface {
            path: vec![0],
            vid: COMPX,
            pid,
            product: product.into(),
            serial: String::new(),
            usage_page: 0xFF02,
            usage: 0x0002,
            input_len: 17,
            output_len: 17,
            feature_len: 0,
        };
        let mut p = Compx::default();
        assert!(is_receiver("VGN Dragonfly 4K Receiver") && !is_receiver("VGN F1 MOBA"));
        // Ismert pár: a kábeles egér a vevő azonosítóját kapja.
        assert_eq!(p.device_id(&iface(0xF506, "VGN F1 MOBA"), true), "compx-f505-0");
        // Ismeretlen pár: csak ha a vevőjét (PID - 1) már láttuk.
        assert_eq!(p.device_id(&iface(0xF101, "ATK X1"), true), "compx-f101-0");
        p.seen.insert("compx-f100-0".into(), Seen { name: "ATK X1".into(), last: None, from_cable: false });
        assert_eq!(p.device_id(&iface(0xF101, "ATK X1"), true), "compx-f100-0");
        assert_eq!(p.device_id(&iface(0xF505, "VGN Dragonfly 4K Receiver"), false), "compx-f505-0");
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
