//! HyperX (HP) vezeték nélküli fejhallgatók. Referencia: HyperHeadset (Cloud III Wireless,
//! Cloud III S Wireless) és HeadsetControl (Cloud II Wireless, Cloud Alpha 2 Wireless).
//!
//! Mind kimenő reporttal kérdez, és bejövő reportban válaszol; a bájtindexek a report ID-t is
//! számolják (0. bájt). A dongle közben magától is küldhet reportokat (gombok, némítás), ezért a
//! választ a fejléce alapján keressük. Csak olvasó kéréseket küldünk.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::hid::{self, Device, HidInterface};
use super::Provider;
use crate::model::{ChargeState, DeviceKind, DeviceState};

const HP: u16 = 0x03F0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Proto {
    /// Cloud III Wireless: [66 cmd], 62 bájt. 0x82 kapcsolat (r[2] = 1), 0x89 szint (r[4], ha
    /// r[2..4] nem 0 0), 0x8A töltés (r[2]: 0 merül, 1 tölt, 2 tele). A válasz r[1]-e a parancs,
    /// vagy az eseményazonosító (0x0B / 0x0D / 0x0C).
    Cloud3,
    /// Cloud III S Wireless: [0C 02 03 01 00 cmd], 64 bájt. Válasz: r[0] = 0C, r[5] = cmd,
    /// érték r[6] (FF: érvénytelen). 0x02 kapcsolat (2 = csatlakozva), 0x06 szint, 0x48 töltés.
    Cloud3S,
    /// Cloud II Wireless: [06 FF BB cmd]. Válasz ugyanezzel a fejléccel; 0x02 szint r[7],
    /// 0x03 töltés r[4] = 1.
    Cloud2,
    /// Cloud Alpha 2 Wireless (bázisállomás): [52 01] -> [53 01], r[2] = 0 nincs kapcsolat;
    /// [50 02] -> [51 02], szint r[2], r[3] = 1 tölt.
    Alpha2,
}

impl Proto {
    fn target(self) -> (u16, u16) {
        match self {
            Proto::Cloud3 => (0xFF13, 0x0001),
            Proto::Cloud3S => (0x01C0, 0x0001),
            Proto::Cloud2 => (0xFF90, 0x0303),
            Proto::Alpha2 => (0xFF13, 0xFF00),
        }
    }
}

struct Model {
    pid: u16,
    proto: Proto,
    key: &'static str,
    name: &'static str,
}

const MODELS: &[Model] = &[
    Model { pid: 0x05B7, proto: Proto::Cloud3, key: "hyperx-cloud3-wireless", name: "HyperX Cloud III Wireless" },
    Model { pid: 0x0C9D, proto: Proto::Cloud3, key: "hyperx-cloud3-wireless", name: "HyperX Cloud III Wireless" },
    Model { pid: 0x06BE, proto: Proto::Cloud3S, key: "hyperx-cloud3s-wireless", name: "HyperX Cloud III S Wireless" },
    Model { pid: 0x02CC, proto: Proto::Cloud3S, key: "hyperx-cloud3s-wireless", name: "HyperX Cloud III S Wireless" },
    Model { pid: 0x0696, proto: Proto::Cloud2, key: "hyperx-cloud2-wireless", name: "HyperX Cloud II Wireless" },
    Model { pid: 0x018B, proto: Proto::Cloud2, key: "hyperx-cloud2-wireless", name: "HyperX Cloud II Wireless" },
    Model { pid: 0x08BE, proto: Proto::Alpha2, key: "hyperx-cloud-alpha2-wireless", name: "HyperX Cloud Alpha 2 Wireless" },
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reading {
    Level(u8, ChargeState),
    /// A dongle válaszolt, de a fejhallgató ki van kapcsolva vagy hatótávon kívül.
    Offline,
}

/// Töltési állapot a HyperHeadset szerinti kódból (0 merül, 1 tölt, 2 tele).
fn charge(level: u8, status: Option<u8>) -> ChargeState {
    match status {
        Some(0) => ChargeState::Discharging,
        Some(1) if level >= 100 => ChargeState::Full,
        Some(1) => ChargeState::Charging,
        Some(2) => ChargeState::Full,
        _ => ChargeState::Unknown,
    }
}

// Válaszok értelmezése (a report ID-vel együtt). None: nem erre a kérésre jött.

fn cloud3_connected(r: &[u8]) -> Option<bool> {
    (r.len() >= 3 && r[0] == 0x66 && matches!(r[1], 0x82 | 0x0B)).then(|| r[2] == 1)
}

fn cloud3_level(r: &[u8]) -> Option<Option<u8>> {
    if r.len() < 5 || r[0] != 0x66 || !matches!(r[1], 0x89 | 0x0D) {
        return None;
    }
    // Mindkét állapotbájt 0: a fejhallgató nem adott szintet.
    Some((!(r[2] == 0 && r[3] == 0) && r[4] <= 100).then_some(r[4]))
}

fn cloud3_charge(r: &[u8]) -> Option<u8> {
    (r.len() >= 3 && r[0] == 0x66 && matches!(r[1], 0x8A | 0x0C)).then_some(r[2])
}

fn cloud3s_value(r: &[u8], cmd: u8) -> Option<Option<u8>> {
    (r.len() >= 7 && r[0] == 0x0C && r[5] == cmd).then(|| (r[6] != 0xFF).then_some(r[6]))
}

fn cloud2_header(r: &[u8], cmd: u8) -> bool {
    r.len() >= 8 && r[..4] == [0x06, 0xFF, 0xBB, cmd]
}

fn alpha2_header(r: &[u8], cmd: u8, sub: u8) -> bool {
    r.len() >= 4 && r[0] == cmd && r[1] == sub
}

/// Kérés a kimenő report hosszára kiegészítve, aztán az első olyan bejövő report, amire `want`
/// igaz (legfeljebb 600 ms).
fn ask(dev: &Device, i: &HidInterface, req: &[u8], want: impl Fn(&[u8]) -> bool) -> Option<Vec<u8>> {
    let out_len = i.output_len as usize;
    if req.len() > out_len || i.input_len == 0 {
        return None;
    }
    let mut msg = req.to_vec();
    msg.resize(out_len, 0);
    if !dev.write(&msg, 250) {
        return None;
    }
    let deadline = Instant::now() + Duration::from_millis(600);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let r = dev.read(i.input_len as usize, left.as_millis().max(1) as u32)?;
        if want(&r) {
            let hex: Vec<String> = r.iter().take(16).map(|b| format!("{b:02X}")).collect();
            crate::worker::log(&format!("hyperx {:04X} válasz: {}", i.pid, hex.join(" ")));
            return Some(r);
        }
    }
    None
}

/// Egy kollekció kiolvasása. None: nem válaszolt.
fn read(i: &HidInterface, proto: Proto) -> Option<Reading> {
    let dev = Device::open(&i.path)?;
    match proto {
        Proto::Cloud3 => {
            if let Some(r) = ask(&dev, i, &[0x66, 0x82], |r| cloud3_connected(r).is_some()) {
                if cloud3_connected(&r) == Some(false) {
                    return Some(Reading::Offline);
                }
            }
            let r = ask(&dev, i, &[0x66, 0x89], |r| cloud3_level(r).is_some())?;
            let level = cloud3_level(&r)??;
            let status = ask(&dev, i, &[0x66, 0x8A], |r| cloud3_charge(r).is_some()).and_then(|r| cloud3_charge(&r));
            Some(Reading::Level(level, charge(level, status)))
        }
        Proto::Cloud3S => {
            let q = |cmd: u8| {
                let r = ask(&dev, i, &[0x0C, 0x02, 0x03, 0x01, 0x00, cmd], |r| cloud3s_value(r, cmd).is_some())?;
                cloud3s_value(&r, cmd)?
            };
            if let Some(c) = q(0x02) {
                if c != 2 {
                    return Some(Reading::Offline);
                }
            }
            let level = q(0x06).filter(|l| *l <= 100)?;
            Some(Reading::Level(level, charge(level, q(0x48))))
        }
        Proto::Cloud2 => {
            let r = ask(&dev, i, &[0x06, 0xFF, 0xBB, 0x02], |r| cloud2_header(r, 0x02))?;
            let level = r[7];
            if level > 100 {
                return None;
            }
            let status = ask(&dev, i, &[0x06, 0xFF, 0xBB, 0x03], |r| cloud2_header(r, 0x03)).map(|r| (r[4] == 1) as u8);
            Some(Reading::Level(level, charge(level, status)))
        }
        Proto::Alpha2 => {
            if let Some(r) = ask(&dev, i, &[0x52, 0x01], |r| alpha2_header(r, 0x53, 0x01)) {
                if r[2] == 0 {
                    return Some(Reading::Offline);
                }
            }
            let r = ask(&dev, i, &[0x50, 0x02], |r| alpha2_header(r, 0x51, 0x02))?;
            let level = r[2];
            if level > 100 {
                return None;
            }
            Some(Reading::Level(level, charge(level, Some((r[3] == 1) as u8))))
        }
    }
}

struct Seen {
    name: &'static str,
    pid: u16,
    last: Option<(u8, ChargeState)>,
}

#[derive(Default)]
pub struct HyperX {
    seen: HashMap<&'static str, Seen>,
    present: usize,
}

impl Provider for HyperX {
    fn scan(&mut self) -> Vec<DeviceState> {
        use crate::worker::log;
        let all = hid::interfaces();
        let mut found: HashMap<&'static str, Option<Reading>> = HashMap::new();
        for m in MODELS {
            let (page, usage) = m.proto.target();
            let mut present = false;
            let mut reading = None;
            for i in all.iter().filter(|i| i.vid == HP && i.pid == m.pid && i.usage_page == page && i.usage == usage && i.output_len > 0) {
                present = true;
                reading = read(i, m.proto);
                if reading.is_some() {
                    break;
                }
            }
            if !present {
                continue;
            }
            log(&format!("hyperx {:04X} {}: {reading:?}", m.pid, m.name));
            let slot = found.entry(m.key).or_insert(None);
            if !matches!(slot, Some(Reading::Level(..))) && reading.is_some() {
                *slot = reading;
            }
            self.seen.entry(m.key).or_insert(Seen { name: m.name, pid: m.pid, last: None }).pid = m.pid;
        }

        self.present = found.len();
        let mut out: Vec<DeviceState> = Vec::new();
        for (key, seen) in self.seen.iter_mut() {
            let base = DeviceState::disconnected(key, seen.name, DeviceKind::Headset);
            let state = match found.get(key) {
                None => base,
                Some(Some(Reading::Offline)) => DeviceState { hw: Some((HP, seen.pid)), ..base },
                Some(fresh) => {
                    if let Some(Reading::Level(l, c)) = fresh {
                        seen.last = Some((*l, *c));
                    }
                    DeviceState {
                        level: seen.last.map(|(l, _)| l),
                        charge: seen.last.map_or(ChargeState::Unknown, |(_, c)| c),
                        connected: true,
                        hw: Some((HP, seen.pid)),
                        ..base
                    }
                }
            };
            out.push(state);
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Amíg egy dongle látható, percenként: a fejhallgató be- és kikapcsolásáról nem jön
    /// Windows-esemény, a dongle végig csatlakoztatva marad.
    fn poll_interval(&self) -> Option<Duration> {
        (self.present > 0).then_some(Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud3() {
        assert_eq!(cloud3_connected(&[0x66, 0x82, 0x01]), Some(true));
        assert_eq!(cloud3_connected(&[0x66, 0x0B, 0x00]), Some(false));
        assert_eq!(cloud3_connected(&[0x66, 0x89, 0x01]), None);
        assert_eq!(cloud3_level(&[0x66, 0x89, 0x01, 0x00, 72]), Some(Some(72)));
        assert_eq!(cloud3_level(&[0x66, 0x0D, 0x00, 0x01, 15]), Some(Some(15)));
        assert_eq!(cloud3_level(&[0x66, 0x89, 0x00, 0x00, 72]), Some(None));
        assert_eq!(cloud3_level(&[0x66, 0x8A, 0x01, 0x00, 72]), None);
        assert_eq!(cloud3_charge(&[0x66, 0x8A, 0x01]), Some(1));
        assert_eq!(charge(40, Some(1)), ChargeState::Charging);
        assert_eq!(charge(100, Some(1)), ChargeState::Full);
        assert_eq!(charge(100, Some(2)), ChargeState::Full);
        assert_eq!(charge(40, Some(0)), ChargeState::Discharging);
        assert_eq!(charge(40, None), ChargeState::Unknown);
    }

    #[test]
    fn cloud3s() {
        // A krisgfx9/Wireless-Device-Battery-Monitor #146 példái: "... 02 02" csatlakozva, "... 06 27" 39%.
        assert_eq!(cloud3s_value(&[0x0C, 0x02, 0x03, 0x01, 0x00, 0x02, 0x02], 0x02), Some(Some(2)));
        assert_eq!(cloud3s_value(&[0x0C, 0x02, 0x03, 0x01, 0x00, 0x06, 0x27], 0x06), Some(Some(39)));
        assert_eq!(cloud3s_value(&[0x0C, 0x02, 0x03, 0x01, 0x00, 0x06, 0xFF], 0x06), Some(None));
        assert_eq!(cloud3s_value(&[0x0C, 0x02, 0x03, 0x01, 0x00, 0x48, 0x01], 0x06), None);
        assert_eq!(cloud3s_value(&[0x0D, 0x02, 0x03, 0x01, 0x00, 0x06, 0x27], 0x06), None);
    }

    #[test]
    fn keys_share_name() {
        for a in MODELS {
            for b in MODELS.iter().filter(|b| b.key == a.key) {
                assert_eq!(a.name, b.name, "{}", a.key);
            }
            assert_eq!(MODELS.iter().filter(|b| b.pid == a.pid).count(), 1, "PID {:04X} kétszer", a.pid);
        }
    }

    /// A csatlakoztatott HyperX eszközök (`cargo test hyperx_present -- --ignored --nocapture`).
    #[test]
    #[ignore = "a csatlakoztatott hardvertől függ"]
    fn hyperx_present() {
        for i in hid::interfaces().iter().filter(|i| i.vid == HP || i.vid == 0x0951) {
            println!(
                "{:04X}:{:04X} {:?} mi={:?} 0x{:04X}/0x{:04X} in {} out {} feature {}",
                i.vid, i.pid, i.product, i.interface_number(), i.usage_page, i.usage, i.input_len, i.output_len, i.feature_len
            );
        }
        for d in HyperX::default().scan() {
            println!("{}", d.tooltip());
        }
    }
}
