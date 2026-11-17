//! ASUS ROG Omni vevő (0B05:1ACE): egy billentyűzet és egy egér akkuszintje ugyanazon a vevőn.
//! Referencia: rogctl, Voltide, UniversalBatteryMonitor, rog-mouse-battery-indicator.
//!
//! A vevő 2-es interfészén három 64 bájtos kollekció van: 0xFF02 (report 01, párosított
//! eszközök), 0xFF00 (report 02, billentyűzet), 0xFF01 (report 03, egér). Kimenő reporttal
//! kérdezünk, bejövő reportban jön a válasz; a bájtindexek a report ID-t is számolják.
//!
//! - Párosítás: [01 A0 00] -> r[3] darab, r[5..]-től 4 bájtos tételek: PID (LE), report ID, típus
//!   (02/04 billentyűzet, 03/05 egér); 0 PID zárja a listát.
//! - Billentyűzet: [02 12 01] -> szint r[6] %, r[9] = 1 tölt.
//! - Egér: [03 12 07] -> szint r[5] %, r[10] != 0 tölt.
//! - [xx FF AA]: elutasítva (az eszköz alszik vagy ki van kapcsolva); 0% töltés nélkül szintén
//!   alvást jelent. Ilyenkor az utolsó ismert szint marad. Csak olvasó kéréseket küldünk.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::hid::{self, Device, HidInterface};
use super::Provider;
use crate::model::{ChargeState, DeviceKind, DeviceState};

const ASUS: u16 = 0x0B05;
const OMNI: u16 = 0x1ACE;

const PAIRING_PAGE: u16 = 0xFF02;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
enum Slot {
    Keyboard,
    Mouse,
}

impl Slot {
    fn page(self) -> u16 {
        match self {
            Slot::Keyboard => 0xFF00,
            Slot::Mouse => 0xFF01,
        }
    }

    fn request(self) -> [u8; 3] {
        match self {
            Slot::Keyboard => [0x02, 0x12, 0x01],
            Slot::Mouse => [0x03, 0x12, 0x07],
        }
    }

    fn id(self) -> &'static str {
        match self {
            Slot::Keyboard => "rog-omni-keyboard",
            Slot::Mouse => "rog-omni-mouse",
        }
    }

    fn kind(self) -> DeviceKind {
        match self {
            Slot::Keyboard => DeviceKind::Keyboard,
            Slot::Mouse => DeviceKind::Mouse,
        }
    }

    fn fallback_name(self) -> &'static str {
        match self {
            Slot::Keyboard => "ASUS ROG keyboard",
            Slot::Mouse => "ASUS ROG mouse",
        }
    }
}

/// Az Omni vevőn jelentkező eszközök saját PID-je (rogctl models.rs).
const NAMES: &[(u16, &str)] = &[
    (0x1A85, "ASUS ROG Azoth"),
    (0x1B42, "ASUS ROG Azoth Extreme"),
    (0x1CF1, "ASUS ROG Azoth Extreme SE"),
    (0x1AB0, "ASUS ROG Strix Scope II 96 Wireless"),
    (0x1B7A, "ASUS ROG Strix Scope II 96 RX Wireless"),
    (0x1B06, "ASUS ROG Falchion RX Low Profile"),
    (0x1B65, "ASUS ROG Harpe Ace Mini"),
    (0x1A94, "ASUS ROG Harpe Ace Aim Lab Edition"),
    (0x1B68, "ASUS ROG Harpe Ace Extreme"),
    (0x1B69, "ASUS ROG Harpe Ace Extreme"),
    (0x1B1A, "ASUS ROG Keris II Ace"),
    (0x1B18, "ASUS ROG Keris II Ace"),
    (0x1C0E, "ASUS ROG Keris II Origin"),
    (0x1D4E, "ASUS ROG Keris II Origin"),
    (0x1A68, "ASUS ROG Keris Wireless AimPoint"),
    (0x1A6A, "ASUS ROG Keris Wireless AimPoint"),
    (0x1A72, "ASUS ROG Gladius III Wireless AimPoint"),
    (0x1AD7, "ASUS ROG Strix Impact III Wireless"),
];

fn name(slot: Slot, pid: Option<u16>) -> String {
    match pid.and_then(|p| NAMES.iter().find(|(n, _)| *n == p)) {
        Some((_, n)) => (*n).into(),
        None => slot.fallback_name().into(),
    }
}

/// A párosítási lista: (eszköz PID, hely). None: nem erre a kérésre jött a válasz.
fn parse_pairing(r: &[u8]) -> Option<Vec<(u16, Slot)>> {
    if r.len() < 4 || r[0] != 0x01 || r[1] != 0xA0 {
        return None;
    }
    let count = r[3] as usize;
    let mut out = Vec::new();
    for e in r.get(5..)?.chunks_exact(4).take(count) {
        let pid = u16::from_le_bytes([e[0], e[1]]);
        if pid == 0 {
            break;
        }
        match (e[2], e[3]) {
            (0x02, 0x04) => out.push((pid, Slot::Keyboard)),
            (0x03, 0x05) => out.push((pid, Slot::Mouse)),
            _ => {}
        }
    }
    Some(out)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reading {
    Level(u8, ChargeState),
    /// Elutasította a kérést, vagy 0%-ot adott töltés nélkül: alszik vagy ki van kapcsolva.
    Asleep,
}

/// Akkuválasz. None: nem erre a kérésre jött, vagy értelmezhetetlen.
fn parse_battery(slot: Slot, r: &[u8]) -> Option<Reading> {
    let req = slot.request();
    if r.len() < 11 || r[0] != req[0] {
        return None;
    }
    if r[1] == 0xFF && r[2] == 0xAA {
        return Some(Reading::Asleep);
    }
    if r[1..3] != req[1..3] {
        return None;
    }
    let (level, charging) = match slot {
        Slot::Keyboard => (r[6], r[9] == 1),
        Slot::Mouse => (r[5], r[10] != 0),
    };
    if level > 100 {
        return None;
    }
    if level == 0 && !charging {
        return Some(Reading::Asleep);
    }
    let charge = match (charging, level) {
        (true, 100) => ChargeState::Full,
        (true, _) => ChargeState::Charging,
        (false, _) => ChargeState::Discharging,
    };
    Some(Reading::Level(level, charge))
}

/// Kérés a kimenő report hosszára kiegészítve, aztán az első bejövő report, amire `want` igaz.
fn ask(i: &HidInterface, req: &[u8], want: impl Fn(&[u8]) -> bool) -> Option<Vec<u8>> {
    let out_len = i.output_len as usize;
    if req.len() > out_len || i.input_len == 0 {
        return None;
    }
    let dev = Device::open(&i.path)?;
    let mut msg = req.to_vec();
    msg.resize(out_len, 0);
    if !dev.write(&msg, 250) {
        return None;
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let r = dev.read(i.input_len as usize, left.as_millis().max(1) as u32)?;
        if want(&r) {
            let hex: Vec<String> = r.iter().take(16).map(|b| format!("{b:02X}")).collect();
            crate::worker::log(&format!("rog omni válasz: {}", hex.join(" ")));
            return Some(r);
        }
    }
    None
}

struct Seen {
    name: String,
    last: Option<(u8, ChargeState)>,
}

#[derive(Default)]
pub struct AsusRog {
    seen: HashMap<Slot, Seen>,
    present: bool,
}

impl Provider for AsusRog {
    fn scan(&mut self) -> Vec<DeviceState> {
        use crate::worker::log;
        let all = hid::interfaces();
        let omni: Vec<&HidInterface> = all.iter().filter(|i| i.vid == ASUS && i.pid == OMNI && i.output_len > 0).collect();
        self.present = !omni.is_empty();
        let channel = |page: u16| omni.iter().copied().find(|i| i.usage_page == page);

        // Hely -> (eszköz PID, ha a párosítási lista ismert; friss olvasás).
        let mut found: HashMap<Slot, (Option<u16>, Option<Reading>)> = HashMap::new();
        if self.present {
            let paired = channel(PAIRING_PAGE).and_then(|i| ask(i, &[0x01, 0xA0, 0x00], |r| parse_pairing(r).is_some())).and_then(|r| parse_pairing(&r));
            log(&format!("rog omni párosítva: {paired:?}"));
            let slots: Vec<(Option<u16>, Slot)> = match &paired {
                Some(list) => list.iter().map(|(p, s)| (Some(*p), *s)).collect(),
                // A lista nélkül mindkét helyet megkérdezzük; csak a szintet adó számít jelenlévőnek.
                None => vec![(None, Slot::Keyboard), (None, Slot::Mouse)],
            };
            for (pid, slot) in slots {
                let reading = channel(slot.page()).and_then(|i| ask(i, &slot.request(), |r| parse_battery(slot, r).is_some())).and_then(|r| parse_battery(slot, &r));
                log(&format!("rog omni {slot:?} {pid:04X?}: {reading:?}"));
                if paired.is_none() && !matches!(reading, Some(Reading::Level(..))) && !self.seen.contains_key(&slot) {
                    continue;
                }
                found.insert(slot, (pid, reading));
            }
        }

        let mut out = Vec::new();
        for (slot, (pid, reading)) in &found {
            let seen = self.seen.entry(*slot).or_insert(Seen { name: name(*slot, *pid), last: None });
            if pid.is_some() {
                seen.name = name(*slot, *pid);
            }
            if let Some(Reading::Level(l, c)) = reading {
                seen.last = Some((*l, *c));
            }
        }
        for (slot, seen) in &self.seen {
            let base = DeviceState::disconnected(slot.id(), &seen.name, slot.kind());
            out.push(match found.get(slot) {
                None => base,
                Some(_) => DeviceState {
                    level: seen.last.map(|(l, _)| l),
                    charge: seen.last.map_or(ChargeState::Unknown, |(_, c)| c),
                    connected: true,
                    hw: Some((ASUS, OMNI)),
                    ..base
                },
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Amíg a vevő látható, percenként: az eszközök szintváltozásáról nem jön Windows-esemény.
    fn poll_interval(&self) -> Option<Duration> {
        self.present.then_some(Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing() {
        // Voltide mintája: Strix Scope II 96 (1AB0) billentyűzet és Harpe Ace Aim Lab (1A94) egér.
        let r = [0x01, 0xA0, 0x00, 0x02, 0x00, 0xB0, 0x1A, 0x02, 0x04, 0x94, 0x1A, 0x03, 0x05, 0, 0, 0, 0];
        assert_eq!(parse_pairing(&r), Some(vec![(0x1AB0, Slot::Keyboard), (0x1A94, Slot::Mouse)]));
        // rogctl mintája: csak egy Falchion RX Low Profile.
        let r = [0x01, 0xA0, 0x00, 0x01, 0x00, 0x06, 0x1B, 0x02, 0x04, 0, 0, 0, 0];
        assert_eq!(parse_pairing(&r), Some(vec![(0x1B06, Slot::Keyboard)]));
        assert_eq!(parse_pairing(&[0x02, 0x12, 0x01, 0x00]), None);
        assert_eq!(name(Slot::Keyboard, Some(0x1AB0)), "ASUS ROG Strix Scope II 96 Wireless");
        assert_eq!(name(Slot::Mouse, Some(0x9999)), "ASUS ROG mouse");
    }

    #[test]
    fn battery() {
        // rogctl felvételei.
        let kb = [0x02, 0x12, 0x01, 0x00, 0x00, 0x00, 0x34, 0x02, 0x01, 0x00, 0x14, 0x34, 0xFF, 0x0E];
        assert_eq!(parse_battery(Slot::Keyboard, &kb), Some(Reading::Level(52, ChargeState::Discharging)));
        let mouse = [0x03, 0x12, 0x07, 0x00, 0x00, 0x50, 0x02, 0x14, 0xD8, 0x0F, 0x00, 0x00, 0x01];
        assert_eq!(parse_battery(Slot::Mouse, &mouse), Some(Reading::Level(80, ChargeState::Discharging)));
        let mut charging = mouse;
        charging[10] = 1;
        assert_eq!(parse_battery(Slot::Mouse, &charging), Some(Reading::Level(80, ChargeState::Charging)));
        let rejected = [0x03, 0xFF, 0xAA, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(parse_battery(Slot::Mouse, &rejected), Some(Reading::Asleep));
        let empty = [0x03, 0x12, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(parse_battery(Slot::Mouse, &empty), Some(Reading::Asleep));
        // A billentyűzet válasza nem az egéré.
        assert_eq!(parse_battery(Slot::Mouse, &kb), None);
    }

    /// Az Omni vevő kollekciói és az olvasás (`cargo test rog_present -- --ignored --nocapture`).
    #[test]
    #[ignore = "a csatlakoztatott hardvertől függ"]
    fn rog_present() {
        for i in hid::interfaces().iter().filter(|i| i.vid == ASUS) {
            println!(
                "{:04X}:{:04X} {:?} mi={:?} 0x{:04X}/0x{:04X} in {} out {} feature {}",
                i.vid, i.pid, i.product, i.interface_number(), i.usage_page, i.usage, i.input_len, i.output_len, i.feature_len
            );
        }
        for d in AsusRog::default().scan() {
            println!("{}", d.tooltip());
        }
    }
}
