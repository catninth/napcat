//! MCHOSE egerek (A7 V2 Pro/Pro+/Ultra/Ultra+): akkuszint kábelen és a vevőn keresztül, az
//! MCHOSE webes driver (www.mchose.com.cn) protokollja szerint.
//!
//! A 0x11-es feature report a 0xFF01/0xFF0B kollekción. A report ID utáni bájtok invertálva
//! (XOR 0xFF) mennek és jönnek. Kérés: [0x06, 0 x 19] (eszközadatok). Az egér egy "e5 06" input
//! reporttal jelzi, hogy kész a válasz, ami utána feature reportként olvasható ki:
//! [0x11, parancs, vid (2, LE), pid (2, LE), firmware (4), kapcsolat, szint %, töltés]. A
//! kapcsolat bájt alsó 3 bitje a mód (0: kábel, 1: vezeték nélküli). 0% szint: az egér alszik
//! vagy nem érhető el; ilyenkor az utolsó ismert szint marad. Csak ezt az olvasó parancsot küldjük.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::hid::{self, Device, HidInterface};
use super::Provider;
use crate::model::{ChargeState, DeviceKind, DeviceState};

/// (VID, PID): kábeles egerek és vevők, ahogy a webes driver felsorolja őket.
const SUPPORTED: &[(u16, u16)] = &[
    (0x3837, 0x4018), // A7 V2 Pro, kábel
    (0x3837, 0x4023), // A7 V2 Pro+, kábel
    (0x3837, 0x4019), // A7 V2 Ultra, kábel
    (0x3837, 0x4021), // A7 V2 Ultra+, kábel
    (0x3837, 0x100A), // 1K vevő
    (0x3837, 0x100B), // 8K vevő
    (0x5253, 0x1020), // 8K vevő
];

const REPORT: u8 = 0x11;
const GET_DEVICE_INFO: u8 = 0x06;
/// Report ID + 20 bájt.
const REPORT_LEN: usize = 21;

#[derive(Debug, PartialEq, Eq)]
struct Info {
    level: u8,
    wired: bool,
    charge: ChargeState,
    /// Az egér saját PID-je (vevőn keresztül is az egéré, nem a vevőé).
    pid: u16,
}

/// A kérés: a report ID utáni bájtok invertálva, a kollekció feature hosszára kiegészítve.
fn request(cmd: u8, len: usize) -> Vec<u8> {
    let mut f = vec![0xFFu8; len.max(REPORT_LEN)];
    f[0] = REPORT;
    f[1] = !cmd;
    f
}

/// Az eszközadat-válasz (a report ID-vel együtt). None: más parancs válasza, vagy alszik az egér.
fn parse_info(r: &[u8]) -> Option<Info> {
    if r.len() < 13 || r[0] != REPORT || !r[1] != GET_DEVICE_INFO {
        return None;
    }
    let p: Vec<u8> = r[2..13].iter().map(|b| !b).collect();
    let pid = u16::from_le_bytes([p[2], p[3]]);
    let (mode, level, charging) = (p[8] & 0x07, p[9], p[10] == 1);
    if pid == 0 || level == 0 {
        return None;
    }
    let level = level.min(100);
    let wired = mode == 0;
    // A driver kijelzése szerint: töltés jelzés vezeték nélkül (dokkon), kábelen 95% alatt mindig.
    let charge = if charging || wired {
        if level >= if wired { 95 } else { 100 } { ChargeState::Full } else { ChargeState::Charging }
    } else {
        ChargeState::Discharging
    };
    Some(Info { level, wired, charge, pid })
}

/// Az "e5 06" input report: kész a válasz.
fn is_ready(r: &[u8]) -> bool {
    r.len() >= 3 && r[1] == !0xE5 && r[2] == !GET_DEVICE_INFO
}

fn read(i: &HidInterface) -> Option<Info> {
    use crate::worker::log;
    let dev = Device::open(&i.path)?;
    let len = (i.feature_len as usize).max(REPORT_LEN);
    for _ in 0..2 {
        if !dev.set_feature(&request(GET_DEVICE_INFO, len)) {
            log("mchose: a kérés nem ment el");
            return None;
        }
        // A jelzés más kollekción is érkezhet, ezért nem várunk rá a végtelenségig.
        let deadline = Instant::now() + Duration::from_millis(300);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match dev.read(i.input_len.max(3) as usize, left.as_millis().max(1) as u32) {
                Some(r) if is_ready(&r) => break,
                Some(_) => {}
                None => break,
            }
        }
        for _ in 0..6 {
            let Some(r) = dev.get_feature(REPORT, len) else { break };
            if !r[1] == GET_DEVICE_INFO {
                let info = parse_info(&r);
                log(&format!("mchose: {info:?}"));
                return info;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    log("mchose: nincs válasz");
    None
}

/// "MCHOSE A7 V2 Ultra+" marad; a vevő általános neve helyett "MCHOSE egér".
fn device_name(product: &str) -> String {
    let name = product.trim();
    let name = ["8K Receiver", "4K Receiver", "1K Receiver", "Receiver", "Dongle"]
        .iter()
        .find_map(|s| name.strip_suffix(s))
        .unwrap_or(name)
        .trim();
    if name.is_empty() || name.eq_ignore_ascii_case("MCHOSE") { crate::i18n::t("MCHOSE egér", "MCHOSE mouse").into() } else { name.into() }
}

/// Kábelen és vevőn ugyanaz a termék név, így a két kapcsolat egy eszköz marad.
fn device_id(name: &str) -> String {
    let name = name.strip_prefix("MCHOSE").unwrap_or(name);
    let slug: String = name.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else if c == '+' { 'p' } else { '-' }).collect();
    format!("mchose-{}", slug.trim_matches('-'))
}

/// Az egér neve a saját PID-je alapján (a vevő termékneve általános, pl. "MCHOSE 8K Receiver").
fn model_name(pid: u16) -> Option<&'static str> {
    Some(match pid {
        0x4018 => "MCHOSE A7 V2 Pro",
        0x4023 => "MCHOSE A7 V2 Pro+",
        0x4019 => "MCHOSE A7 V2 Ultra",
        0x4021 => "MCHOSE A7 V2 Ultra+",
        _ => return None,
    })
}

#[derive(Default)]
pub struct Mchose {
    /// Eszközönként: név és az utolsó ismert állapot (alvás közben ez marad).
    seen: HashMap<String, (String, Option<Info>)>,
    /// Vevő (VID, PID) -> a rajta legutóbb válaszoló egér PID-je; alvás közben is ehhez kötjük.
    links: HashMap<(u16, u16), u16>,
    connected: usize,
    any_unknown: bool,
}

impl Provider for Mchose {
    fn scan(&mut self) -> Vec<DeviceState> {
        let mut out: Vec<DeviceState> = Vec::new();
        // HID eszközönként (vevő vagy kábeles egér) a megfelelő kollekciók; az első válaszoló számít.
        let mut groups: Vec<((u16, u16), String, Vec<HidInterface>)> = Vec::new();
        for i in hid::interfaces() {
            if !SUPPORTED.contains(&(i.vid, i.pid)) || !matches!(i.usage_page, 0xFF01 | 0xFF0B) || (i.feature_len as usize) < REPORT_LEN {
                continue;
            }
            match groups.iter_mut().find(|g| g.0 == (i.vid, i.pid)) {
                Some(g) => g.2.push(i),
                None => groups.push(((i.vid, i.pid), i.product.clone(), vec![i])),
            }
        }
        // Egerenként (a vevőn és a kábelen át is ugyanaz): név, friss adat, VID/PID; a kábel nyer.
        let mut mice: Vec<(String, String, Option<Info>, (u16, u16))> = Vec::new();
        for (hw, product, mut interfaces) in groups {
            // A 0xFF01 válaszol (A7 V2 Ultra+ 8K vevőn ellenőrizve), a 0xFF0B elutasítja a kérést.
            interfaces.sort_by_key(|i| i.usage_page != 0xFF01);
            let fresh = interfaces.iter().find_map(read);
            let cable = model_name(hw.1).is_some();
            let mouse = if cable {
                Some(hw.1)
            } else {
                let answered = fresh.as_ref().map(|f| f.pid).filter(|p| model_name(*p).is_some());
                if let Some(p) = answered {
                    // Mostantól ismert, melyik egér van a vevőn: az általános bejegyzés megy.
                    self.links.insert(hw, p);
                    self.seen.remove(&device_id(&device_name(&product)));
                }
                self.links.get(&hw).copied()
            };
            let name = mouse.and_then(model_name).map(String::from).unwrap_or_else(|| device_name(&product));
            let id = device_id(&name);
            match mice.iter_mut().find(|m| m.0 == id) {
                Some(m) => {
                    if fresh.is_some() && (m.2.is_none() || cable) {
                        (m.2, m.3) = (fresh, hw);
                    }
                }
                None => mice.push((id, name, fresh, hw)),
            }
        }
        for (id, name, fresh, hw) in mice {
            let entry = self.seen.entry(id.clone()).or_insert_with(|| (name, None));
            match fresh {
                Some(f) => entry.1 = Some(f),
                // Kihúzott kábel után, amíg a vevőn alvó egér nem válaszol: a szint marad, a töltés nem.
                None if entry.1.as_ref().is_some_and(|f| f.wired) && model_name(hw.1).is_none() => {
                    if let Some(f) = entry.1.as_mut() {
                        f.wired = false;
                        f.charge = ChargeState::Discharging;
                    }
                }
                None => {}
            }
            let (level, charge) = match &entry.1 {
                Some(f) => (Some(f.level), f.charge),
                None => (None, ChargeState::Unknown),
            };
            out.push(DeviceState {
                level,
                charge,
                connected: true,
                hw: Some(hw),
                ..DeviceState::disconnected(&id, &entry.0, DeviceKind::Mouse)
            });
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

    /// Válasz a report ID után invertált adatokkal.
    fn response(payload: &[u8]) -> Vec<u8> {
        let mut r = vec![REPORT, !GET_DEVICE_INFO];
        r.extend(payload.iter().map(|b| !b));
        r.resize(REPORT_LEN, 0xFF);
        r
    }

    #[test]
    fn real_capture() {
        // A7 V2 Ultra+ a 8K vevőn (3837:100B, 0xFF01), napcat-diag kimenetéből.
        let ack = [0x13, 0x1A, 0xF9, 0xFF, 0xFF];
        assert!(is_ready(&ack));
        let resp = [0x11, 0xF9, 0xC8, 0xC7, 0xDE, 0xBF, 0xFA, 0xD1, 0xFD, 0xFB, 0xF6, 0x9B, 0xFF, 0x9B, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        assert_eq!(parse_info(&resp), Some(Info { level: 100, wired: false, charge: ChargeState::Discharging, pid: 0x4021 }));
    }

    #[test]
    fn request_and_info() {
        let req = request(GET_DEVICE_INFO, 21);
        assert_eq!(req.len(), 21);
        assert_eq!(&req[..3], &[0x11, 0xF9, 0xFF]);
        assert!(req[2..].iter().all(|&b| b == 0xFF));
        assert_eq!(request(GET_DEVICE_INFO, 65).len(), 65);

        // Vevőn: vid 3837, pid 100B, 72%, nem tölt.
        let wireless = response(&[0x37, 0x38, 0x0B, 0x10, 1, 2, 3, 4, 0b0000_1001, 72, 0]);
        assert_eq!(parse_info(&wireless), Some(Info { level: 72, wired: false, charge: ChargeState::Discharging, pid: 0x100B }));
        // Dokkon tölt.
        let docked = response(&[0x37, 0x38, 0x0B, 0x10, 0, 0, 0, 0, 0b0000_1001, 80, 1]);
        assert_eq!(parse_info(&docked).map(|i| i.charge), Some(ChargeState::Charging));
        // Kábelen, tele.
        let wired = response(&[0x37, 0x38, 0x21, 0x40, 0, 0, 0, 0, 0b0000_1000, 100, 0]);
        assert_eq!(parse_info(&wired), Some(Info { level: 100, wired: true, charge: ChargeState::Full, pid: 0x4021 }));
        // Alvó egér, és más parancs válasza.
        assert_eq!(parse_info(&response(&[0x37, 0x38, 0x0B, 0x10, 0, 0, 0, 0, 1, 0, 0])), None);
        let mut other = wireless.clone();
        other[1] = !0x04;
        assert_eq!(parse_info(&other), None);

        assert!(is_ready(&[0x11, 0x1A, 0xF9, 0xFF]));
        assert!(!is_ready(&[0x11, 0x1D, 0xF9]));
        assert_eq!(device_name("MCHOSE A7 V2 Ultra+"), "MCHOSE A7 V2 Ultra+");
        assert_eq!(device_id("MCHOSE A7 V2 Ultra+"), "mchose-a7-v2-ultrap");
    }

    /// `cargo test mchose_present -- --ignored --nocapture` (mozgasd közben az egeret).
    #[test]
    #[ignore = "a csatlakoztatott hardvertől függ"]
    fn mchose_present() {
        for i in hid::interfaces().iter().filter(|i| matches!(i.vid, 0x3837 | 0x5253)) {
            println!("{:04X}:{:04X} {:?} 0x{:04X}/0x{:04X} in {} feature {}", i.vid, i.pid, i.product, i.usage_page, i.usage, i.input_len, i.feature_len);
        }
        let mut p = Mchose::default();
        for d in p.scan() {
            println!("{}", d.tooltip());
        }
    }
}
