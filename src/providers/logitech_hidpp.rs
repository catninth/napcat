//! Logitech HID++ 2.0 eszközök: Unifying, Bolt és Lightspeed vevőn párosított, illetve kábellel
//! közvetlenül csatlakozó egerek és billentyűzetek. Referencia: Solaar.
//!
//! Csak olvasó kéréseket küld (protokollverzió, funkciókeresés, név, típus, akku). Windows-on a
//! HID++ hosszú (20 bájtos, 0x11) reportjai a 0xFF00/0x0002 kollekcióban mennek; csak oda írunk,
//! így pl. a G435 vevőjének más formátumú 0xFF00/0x0001 csatornája kimarad.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::hid::{self, Device, HidInterface};
use super::Provider;
use crate::model::{ChargeState, DeviceKind, DeviceState};

const LOGITECH: u16 = 0x046D;
const LONG: u8 = 0x11;
/// Szoftverazonosító a kéréseinkben: így a válaszaink elkülönülnek az eszköz saját értesítéseitől.
const SWID: u8 = 0x0A;
/// Közvetlenül csatlakozó eszköz (kábel, Bluetooth) indexe; vevőn a párosított eszközök 1-6.
const DIRECT: u8 = 0xFF;

const ROOT: u8 = 0x00;
const FEATURE_DEVICE_INFO: u16 = 0x0003;
const FEATURE_NAME: u16 = 0x0005;
const FEATURE_BATTERY_STATUS: u16 = 0x1000;
const FEATURE_BATTERY_VOLTAGE: u16 = 0x1001;
const FEATURE_UNIFIED_BATTERY: u16 = 0x1004;

/// Egy kérés-válasz: a válasz 16 bájtnyi paramétere, vagy None hibánál / időtúllépésnél.
fn request(dev: &Device, index: u8, feature: u8, function: u8, params: &[u8]) -> Option<[u8; 16]> {
    let mut msg = [0u8; 20];
    msg[0] = LONG;
    msg[1] = index;
    msg[2] = feature;
    msg[3] = (function << 4) | SWID;
    msg[4..4 + params.len()].copy_from_slice(params);
    if !dev.write(&msg, 200) {
        return None;
    }
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        let left = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
        let r = dev.read(20, left.max(1))?;
        if r.len() < 20 || r[0] != LONG || r[1] != index {
            continue; // más eszköz vagy értesítés
        }
        if r[2] == 0xFF && r[3] == feature && r[4] == msg[3] {
            return None; // HID++ 2.0 hibaválasz
        }
        if r[2] == feature && r[3] == msg[3] {
            let mut out = [0u8; 16];
            out.copy_from_slice(&r[4..20]);
            return Some(out);
        }
    }
}

/// A funkció indexe az eszközön (Root.getFeature), ha támogatja.
fn feature_index(dev: &Device, index: u8, feature: u16) -> Option<u8> {
    let r = request(dev, index, ROOT, 0, &feature.to_be_bytes())?;
    (r[0] != 0).then_some(r[0])
}

/// HID++ 2.0 vagy újabb eszköz-e (Root.getProtocolVersion; az 1.0-s eszközök hibát adnak).
fn is_hidpp2(dev: &Device, index: u8) -> bool {
    request(dev, index, ROOT, 1, &[0, 0, 0x5A]).is_some_and(|r| r[0] >= 2)
}

/// UNIFIED_BATTERY.getStatus: töltöttség %, szintjelzők, töltési állapot.
fn parse_unified(r: &[u8]) -> (u8, bool, ChargeState) {
    let (level, approx) = if r[0] > 0 {
        (r[0].min(100), false)
    } else {
        // Csak szintjelzőket támogató eszköz: teli / jó / alacsony / kritikus.
        let flags = r[1];
        let level = if flags & 0x08 != 0 {
            100
        } else if flags & 0x04 != 0 {
            60
        } else if flags & 0x02 != 0 {
            20
        } else {
            5
        };
        (level, true)
    };
    let charge = match r[2] {
        0 => ChargeState::Discharging,
        1 | 2 => ChargeState::Charging,
        3 => ChargeState::Full,
        _ => ChargeState::Unknown,
    };
    (level, approx, charge)
}

/// BATTERY_STATUS.getBatteryLevelStatus: szint %, következő szint, állapot.
fn parse_status(r: &[u8]) -> Option<(u8, bool, ChargeState)> {
    let charge = match r[2] {
        0 => ChargeState::Discharging,
        1 | 2 | 4 => ChargeState::Charging,
        3 => ChargeState::Full,
        _ => return None, // érvénytelen akku, hőmérséklet- vagy töltési hiba
    };
    let level = if charge == ChargeState::Full { 100 } else { r[0].min(100) };
    Some((level, false, charge))
}

/// BATTERY_VOLTAGE.getBatteryInfo: feszültség (mV) és jelzők; a szint egy Li-ion görbéből becsült.
fn parse_voltage(r: &[u8]) -> (u8, bool, ChargeState) {
    let mv = u16::from_be_bytes([r[0], r[1]]);
    let flags = r[2];
    let charge = if flags & 0x80 != 0 {
        match flags & 0x07 {
            0 | 1 => ChargeState::Charging,
            2 => ChargeState::Full,
            _ => ChargeState::Unknown,
        }
    } else {
        ChargeState::Discharging
    };
    (voltage_to_percent(mv), true, charge)
}

fn voltage_to_percent(mv: u16) -> u8 {
    // Tipikus egycellás Li-ion kisütési görbe (mV, %), csökkenő sorrendben.
    const CURVE: [(u16, u8); 11] = [
        (4186, 100),
        (4067, 90),
        (3989, 80),
        (3922, 70),
        (3859, 60),
        (3811, 50),
        (3778, 40),
        (3751, 30),
        (3717, 20),
        (3671, 10),
        (3500, 0),
    ];
    if mv >= CURVE[0].0 {
        return 100;
    }
    for w in CURVE.windows(2) {
        let ((hi_mv, hi_p), (lo_mv, lo_p)) = (w[0], w[1]);
        if mv >= lo_mv {
            let t = (mv - lo_mv) as f32 / (hi_mv - lo_mv) as f32;
            return (lo_p as f32 + t * (hi_p - lo_p) as f32).round() as u8;
        }
    }
    0
}

fn battery(dev: &Device, index: u8) -> Option<(u8, bool, ChargeState)> {
    if let Some(fi) = feature_index(dev, index, FEATURE_UNIFIED_BATTERY) {
        return Some(parse_unified(&request(dev, index, fi, 1, &[])?));
    }
    if let Some(fi) = feature_index(dev, index, FEATURE_BATTERY_STATUS) {
        return parse_status(&request(dev, index, fi, 0, &[])?);
    }
    if let Some(fi) = feature_index(dev, index, FEATURE_BATTERY_VOLTAGE) {
        return Some(parse_voltage(&request(dev, index, fi, 0, &[])?));
    }
    None
}

/// Név és típus (DEVICE_NAME); a típuskódok: 0 billentyűzet, 3 egér, 4 touchpad, 5 trackball, 8 headset.
fn name_and_kind(dev: &Device, index: u8) -> Option<(String, DeviceKind)> {
    let fi = feature_index(dev, index, FEATURE_NAME)?;
    let len = request(dev, index, fi, 0, &[])?[0] as usize;
    let mut name = Vec::new();
    while name.len() < len.min(64) {
        let chunk = request(dev, index, fi, 1, &[name.len() as u8])?;
        name.extend(chunk.iter().take(len - name.len()).take_while(|&&b| b != 0));
        if chunk[0] == 0 {
            break;
        }
    }
    let kind = match request(dev, index, fi, 2, &[]).map(|r| r[0]) {
        Some(0 | 2) => DeviceKind::Keyboard,
        Some(3..=5) => DeviceKind::Mouse,
        Some(8) => DeviceKind::Headset,
        _ => DeviceKind::Other,
    };
    Some((String::from_utf8_lossy(&name).trim().to_string(), kind))
}

/// Stabil azonosító: az eszköz egyedi azonosítója (DEVICE_INFO unitId), ha van.
fn unit_id(dev: &Device, index: u8) -> Option<String> {
    let fi = feature_index(dev, index, FEATURE_DEVICE_INFO)?;
    let r = request(dev, index, fi, 0, &[])?;
    Some(format!("{:02x}{:02x}{:02x}{:02x}", r[1], r[2], r[3], r[4]))
}

fn read_device(dev: &Device, i: &HidInterface, index: u8) -> Option<DeviceState> {
    if !is_hidpp2(dev, index) {
        return None; // üres hely, kikapcsolt eszköz vagy HID++ 1.0
    }
    let (name, kind) = name_and_kind(dev, index).unwrap_or_else(|| (i.product.clone(), DeviceKind::Other));
    let id = match unit_id(dev, index) {
        Some(u) => format!("logi-{u}"),
        None => format!("logi-{:04x}-{}-{index}", i.pid, i.serial),
    };
    let (level, level_approx, charge) = match battery(dev, index) {
        Some((l, a, c)) => (Some(l), a, c),
        None => (None, false, ChargeState::Unknown),
    };
    Some(DeviceState { id, name, kind, level, level_approx, charge, connected: true, hw: Some((i.vid, i.pid)), estimate: None })
}

#[derive(Default)]
pub struct LogitechHidpp {
    /// Az ebben a munkamenetben látott eszközök; eltűnésük vagy kikapcsolásuk után "nincs csatlakoztatva".
    seen: HashMap<String, (String, DeviceKind)>,
    connected: usize,
}

impl Provider for LogitechHidpp {
    fn scan(&mut self) -> Vec<DeviceState> {
        let mut out: Vec<DeviceState> = Vec::new();
        let long = hid::interfaces()
            .into_iter()
            .filter(|i| i.vid == LOGITECH && i.usage_page == 0xFF00 && i.usage == 0x0002 && i.input_len == 20);
        for i in long {
            let Some(dev) = Device::open(&i.path) else { continue };
            // Közvetlen HID++ 2.0 eszköz (kábel), különben vevő: a párosított helyek 1-6.
            let found: Vec<DeviceState> = match read_device(&dev, &i, DIRECT) {
                Some(d) => vec![d],
                None => (1..=6).filter_map(|index| read_device(&dev, &i, index)).collect(),
            };
            for d in found {
                if !out.iter().any(|o| o.id == d.id) {
                    self.seen.insert(d.id.clone(), (d.name.clone(), d.kind));
                    out.push(d);
                }
            }
        }
        self.connected = out.len();
        for (id, (name, kind)) in &self.seen {
            if !out.iter().any(|d| &d.id == id) {
                out.push(DeviceState::disconnected(id, name, *kind));
            }
        }
        out
    }

    /// Az akkuszint lassan változik; 5 percenként, és csak ha van csatlakoztatott eszköz.
    fn poll_interval(&self) -> Option<Duration> {
        (self.connected > 0).then_some(Duration::from_secs(300))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A most csatlakoztatott HID++ eszközök (`cargo test hidpp_present -- --ignored --nocapture`).
    #[test]
    #[ignore = "a csatlakoztatott hardvertől függ"]
    fn hidpp_present() {
        for i in hid::interfaces().iter().filter(|i| i.vid == LOGITECH) {
            let used = i.usage_page == 0xFF00 && i.usage == 0x0002 && i.input_len == 20;
            println!("{:04X}:{:04X} {} page 0x{:04X}/0x{:04X} in {} -> {}", i.vid, i.pid, i.product, i.usage_page, i.usage, i.input_len, if used { "HID++" } else { "kihagyva" });
        }
        let t = Instant::now();
        for d in LogitechHidpp::default().scan() {
            println!("{}", d.tooltip());
        }
        println!("scan(): {:?}", t.elapsed());
    }

    fn params(bytes: &[u8]) -> [u8; 16] {
        let mut r = [0u8; 16];
        r[..bytes.len()].copy_from_slice(bytes);
        r
    }

    #[test]
    fn unified_battery() {
        assert_eq!(parse_unified(&params(&[73, 0x04, 0])), (73, false, ChargeState::Discharging));
        assert_eq!(parse_unified(&params(&[40, 0x04, 1])), (40, false, ChargeState::Charging));
        assert_eq!(parse_unified(&params(&[100, 0x08, 3])), (100, false, ChargeState::Full));
        // Csak szintjelző: alacsony.
        assert_eq!(parse_unified(&params(&[0, 0x02, 0])), (20, true, ChargeState::Discharging));
    }

    #[test]
    fn battery_status() {
        assert_eq!(parse_status(&params(&[50, 20, 0])), Some((50, false, ChargeState::Discharging)));
        assert_eq!(parse_status(&params(&[90, 50, 3])), Some((100, false, ChargeState::Full)));
        assert_eq!(parse_status(&params(&[0, 0, 5])), None);
    }

    #[test]
    fn battery_voltage() {
        assert_eq!(voltage_to_percent(4250), 100);
        assert_eq!(voltage_to_percent(3811), 50);
        assert_eq!(voltage_to_percent(3400), 0);
        let (p, approx, c) = parse_voltage(&params(&[0x0F, 0x50, 0x00])); // 3920 mV
        assert!((68..=70).contains(&p) && approx && c == ChargeState::Discharging);
        assert_eq!(parse_voltage(&params(&[0x10, 0x5A, 0x80])).2, ChargeState::Charging);
    }
}
