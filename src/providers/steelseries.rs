//! SteelSeries vezeték nélküli fejhallgatók (Arctis, Arctis Nova, GameBuds) és egerek (Aerox,
//! Prime, Rival). Referencia: HeadsetControl (fejhallgatók) és rivalcfg (egerek).
//!
//! Mindkét projekt hidapi-n át ír és olvas, így a bájtpozíciók a hidapi Windows-os viselkedéséhez
//! igazodnak: íráskor a report a kimenő report hosszára egészül ki, olvasáskor a 0-s report ID
//! (számozatlan report) lemarad az elejéről. A `transact` ugyanígy dolgozik. Csak olvasó kéréseket
//! küldünk (állapot / akkuszint).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::hid::{self, Device, HidInterface};
use super::Provider;
use crate::model::{ChargeState, DeviceKind, DeviceState};

const STEELSERIES: u16 = 0x1038;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Proto {
    /// Arctis Nova 7 család: [00 b0] -> szint d[2] (régi firmware 0-4, új 0-100), állapot d[3].
    Nova7 { discrete: bool },
    /// Arctis Nova 5, Nova 3P/3X Wireless: [00 b0] -> d[1] = 2 nincs kapcsolat, szint d[3], d[4] = 1 tölt.
    Nova5,
    /// Arctis 7+: [00 b0] -> d[1] = 1 nincs kapcsolat, szint d[2] (0-4), d[3] = 1 tölt.
    Arctis7Plus,
    /// Arctis GameBuds: [00 b0] -> fülhallgatónként állapot d[3]/d[4] (3 = tokon kívül), szint d[5]/d[6].
    GameBuds,
    /// Arctis Nova Pro Wireless (bázisállomás): [06 b0] -> szint r[6] (0-8), állapot r[15].
    NovaPro,
    /// Arctis 7 (2017, 2019): [06 18] -> szint r[2] %.
    Arctis7,
    /// Arctis 1 Wireless, Arctis 7P/7X: [06 12] -> r[2] = 1 nincs kapcsolat, szint r[3] %.
    Arctis1,
    /// Arctis 9: [00 20] -> szint r[3] (0x64-0x9A), r[4] = 1 tölt.
    Arctis9,
    /// Arctis Pro Wireless: [41 aa] -> d[0] = 2 nincs kapcsolat; [40 aa] -> szint d[0] (0-4).
    ProWireless,
    /// Aerox 3/5/9 Wireless, Prime Wireless: [92] (kábel) / [d2] (vevő) -> d[1]: 0x80 tölt, (x - 1) * 5 %.
    Aerox { wireless: bool },
    /// Rival 3 Wireless, Rival 650: [aa 01] -> szint d[0] %, d[2] != 0 tölt.
    Rival { interface: u8 },
}

impl Proto {
    /// Melyik HID kollekción megy a kérés.
    fn target(self) -> Target {
        match self {
            Proto::Nova7 { .. } | Proto::Nova5 | Proto::Arctis7Plus | Proto::GameBuds => Target::Usage(0xFFC0, 0x0001),
            Proto::Arctis7 | Proto::Arctis1 => Target::Usage(0xFF43, 0x0202),
            Proto::NovaPro => Target::Interface(4),
            Proto::Arctis9 | Proto::ProWireless => Target::Interface(0),
            Proto::Aerox { .. } => Target::Interface(3),
            Proto::Rival { interface } => Target::Interface(interface),
        }
    }

    /// A kérés, és amivel a válasz várhatóan kezdődik (a többi bejövő reporttal szemben ezt
    /// részesítjük előnyben; ha ilyen nem jön, az első válasz számít).
    fn request(self) -> (&'static [u8], &'static [u8]) {
        match self {
            Proto::Nova7 { .. } | Proto::Nova5 | Proto::Arctis7Plus | Proto::GameBuds => (&[0x00, 0xB0], &[0xB0]),
            Proto::NovaPro => (&[0x06, 0xB0], &[0x06, 0xB0]),
            Proto::Arctis7 => (&[0x06, 0x18], &[0x06, 0x18]),
            Proto::Arctis1 => (&[0x06, 0x12], &[0x06, 0x12]),
            Proto::Arctis9 => (&[0x00, 0x20], &[]),
            Proto::ProWireless => (&[0x41, 0xAA], &[]),
            Proto::Aerox { wireless: false } => (&[0x00, 0x92], &[0x92]),
            Proto::Aerox { wireless: true } => (&[0x00, 0xD2], &[0xD2]),
            Proto::Rival { .. } => (&[0x00, 0xAA, 0x01], &[]),
        }
    }
}

#[derive(Clone, Copy)]
enum Target {
    Usage(u16, u16),
    Interface(u8),
}

struct Model {
    pid: u16,
    proto: Proto,
    /// Közös azonosító: ugyanaz az eszköz más PID-del is jelentkezhet (kábel / vevő, firmware-frissítés után).
    key: &'static str,
    name: &'static str,
    kind: DeviceKind,
}

const fn headset(pid: u16, proto: Proto, key: &'static str, name: &'static str) -> Model {
    Model { pid, proto, key, name, kind: DeviceKind::Headset }
}

const fn mouse(pid: u16, proto: Proto, key: &'static str, name: &'static str) -> Model {
    Model { pid, proto, key, name, kind: DeviceKind::Mouse }
}

const OLD: Proto = Proto::Nova7 { discrete: true };
const NEW: Proto = Proto::Nova7 { discrete: false };
const AEROX_WIRED: Proto = Proto::Aerox { wireless: false };
const AEROX_WIRELESS: Proto = Proto::Aerox { wireless: true };

const MODELS: &[Model] = &[
    // Arctis Nova 7 család; a 2026 januári firmware után százalékot ad, és más a PID.
    headset(0x2202, OLD, "ss-nova7", "SteelSeries Arctis Nova 7"),
    headset(0x22A1, NEW, "ss-nova7", "SteelSeries Arctis Nova 7"),
    headset(0x227E, NEW, "ss-nova7-gen2", "SteelSeries Arctis Nova 7 Gen 2"),
    headset(0x2206, OLD, "ss-nova7x", "SteelSeries Arctis Nova 7X"),
    headset(0x22A4, OLD, "ss-nova7x", "SteelSeries Arctis Nova 7X"),
    headset(0x22A5, NEW, "ss-nova7x", "SteelSeries Arctis Nova 7X"),
    headset(0x2258, NEW, "ss-nova7x-v2", "SteelSeries Arctis Nova 7X"),
    headset(0x229E, NEW, "ss-nova7x-v2", "SteelSeries Arctis Nova 7X"),
    headset(0x22AD, NEW, "ss-nova7x-v2", "SteelSeries Arctis Nova 7X"),
    headset(0x220A, OLD, "ss-nova7p", "SteelSeries Arctis Nova 7P"),
    headset(0x22A7, NEW, "ss-nova7p", "SteelSeries Arctis Nova 7P"),
    headset(0x2298, NEW, "ss-nova7p-v2", "SteelSeries Arctis Nova 7P"),
    headset(0x223A, OLD, "ss-nova7-diablo", "SteelSeries Arctis Nova 7 Diablo IV"),
    headset(0x22A9, NEW, "ss-nova7-diablo", "SteelSeries Arctis Nova 7 Diablo IV"),
    headset(0x227A, OLD, "ss-nova7-wow", "SteelSeries Arctis Nova 7 WoW Edition"),
    // Arctis Nova 5 és Nova 3P/3X Wireless.
    headset(0x2232, Proto::Nova5, "ss-nova5", "SteelSeries Arctis Nova 5"),
    headset(0x2253, Proto::Nova5, "ss-nova5x", "SteelSeries Arctis Nova 5X"),
    headset(0x2269, Proto::Nova5, "ss-nova3p", "SteelSeries Arctis Nova 3P Wireless"),
    headset(0x226D, Proto::Nova5, "ss-nova3x", "SteelSeries Arctis Nova 3X Wireless"),
    // Arctis 7+.
    headset(0x220E, Proto::Arctis7Plus, "ss-arctis7plus", "SteelSeries Arctis 7+"),
    headset(0x2212, Proto::Arctis7Plus, "ss-arctis7pplus", "SteelSeries Arctis 7P+"),
    headset(0x2216, Proto::Arctis7Plus, "ss-arctis7xplus", "SteelSeries Arctis 7X+"),
    headset(0x2236, Proto::Arctis7Plus, "ss-arctis7plus-destiny", "SteelSeries Arctis 7+ Destiny 2"),
    headset(0x230A, Proto::GameBuds, "ss-gamebuds", "SteelSeries Arctis GameBuds"),
    headset(0x12E0, Proto::NovaPro, "ss-novapro", "SteelSeries Arctis Nova Pro Wireless"),
    headset(0x12E5, Proto::NovaPro, "ss-novapro-x", "SteelSeries Arctis Nova Pro Wireless X"),
    // Régebbi Arctis vezeték nélküliek.
    headset(0x1260, Proto::Arctis7, "ss-arctis7", "SteelSeries Arctis 7"),
    headset(0x12AD, Proto::Arctis7, "ss-arctis7", "SteelSeries Arctis 7"),
    headset(0x12B3, Proto::Arctis1, "ss-arctis1", "SteelSeries Arctis 1 Wireless"),
    headset(0x12B6, Proto::Arctis1, "ss-arctis1-xbox", "SteelSeries Arctis 1 Wireless"),
    headset(0x12D7, Proto::Arctis1, "ss-arctis7x", "SteelSeries Arctis 7X"),
    headset(0x12D5, Proto::Arctis1, "ss-arctis7p", "SteelSeries Arctis 7P"),
    headset(0x12C2, Proto::Arctis9, "ss-arctis9", "SteelSeries Arctis 9"),
    headset(0x12C4, Proto::Arctis9, "ss-arctis9", "SteelSeries Arctis 9"),
    headset(0x1290, Proto::ProWireless, "ss-arctis-pro-wireless", "SteelSeries Arctis Pro Wireless"),
    // Egerek: kábelen és a 2,4 GHz-es vevőn más a PID, de ugyanaz az eszköz.
    mouse(0x183A, AEROX_WIRED, "ss-aerox3", "SteelSeries Aerox 3 Wireless"),
    mouse(0x1838, AEROX_WIRELESS, "ss-aerox3", "SteelSeries Aerox 3 Wireless"),
    mouse(0x187A, AEROX_WIRED, "ss-aerox3-cs2", "SteelSeries Aerox 3 Wireless CS2"),
    mouse(0x1878, AEROX_WIRELESS, "ss-aerox3-cs2", "SteelSeries Aerox 3 Wireless CS2"),
    mouse(0x1854, AEROX_WIRED, "ss-aerox5", "SteelSeries Aerox 5 Wireless"),
    mouse(0x1852, AEROX_WIRELESS, "ss-aerox5", "SteelSeries Aerox 5 Wireless"),
    mouse(0x185E, AEROX_WIRED, "ss-aerox5-destiny", "SteelSeries Aerox 5 Wireless Destiny 2"),
    mouse(0x185C, AEROX_WIRELESS, "ss-aerox5-destiny", "SteelSeries Aerox 5 Wireless Destiny 2"),
    mouse(0x1862, AEROX_WIRED, "ss-aerox5-diablo", "SteelSeries Aerox 5 Wireless Diablo IV"),
    mouse(0x1860, AEROX_WIRELESS, "ss-aerox5-diablo", "SteelSeries Aerox 5 Wireless Diablo IV"),
    mouse(0x185A, AEROX_WIRED, "ss-aerox9", "SteelSeries Aerox 9 Wireless"),
    mouse(0x1858, AEROX_WIRELESS, "ss-aerox9", "SteelSeries Aerox 9 Wireless"),
    mouse(0x1876, AEROX_WIRED, "ss-aerox9-wow", "SteelSeries Aerox 9 Wireless WoW"),
    mouse(0x1874, AEROX_WIRELESS, "ss-aerox9-wow", "SteelSeries Aerox 9 Wireless WoW"),
    mouse(0x1842, AEROX_WIRED, "ss-prime-wireless", "SteelSeries Prime Wireless"),
    mouse(0x1840, AEROX_WIRELESS, "ss-prime-wireless", "SteelSeries Prime Wireless"),
    mouse(0x184A, AEROX_WIRED, "ss-prime-mini-wireless", "SteelSeries Prime Mini Wireless"),
    mouse(0x1848, AEROX_WIRELESS, "ss-prime-mini-wireless", "SteelSeries Prime Mini Wireless"),
    mouse(0x1830, Proto::Rival { interface: 3 }, "ss-rival3-wireless", "SteelSeries Rival 3 Wireless"),
    mouse(0x1872, Proto::Rival { interface: 3 }, "ss-rival3-wireless-gen2", "SteelSeries Rival 3 Wireless Gen 2"),
    mouse(0x172B, Proto::Rival { interface: 0 }, "ss-rival650", "SteelSeries Rival 650 Wireless"),
    mouse(0x1726, Proto::Rival { interface: 0 }, "ss-rival650", "SteelSeries Rival 650 Wireless"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reading {
    /// Szint %, közelítő-e (fokozatokban jön), töltés.
    Level(u8, bool, ChargeState),
    /// A vevő / bázisállomás válaszolt, de a fejhallgató ki van kapcsolva vagy hatótávon kívül.
    Offline,
}

fn level(l: u8, approx: bool, charging: bool) -> Reading {
    let charge = match (charging, l) {
        (true, 100) => ChargeState::Full,
        (true, _) => ChargeState::Charging,
        (false, _) => ChargeState::Discharging,
    };
    Reading::Level(l.min(100), approx, charge)
}

/// Fokozatos szint (0..=max) százalékra.
fn steps(raw: u8, max: u8) -> u8 {
    (raw.min(max) as u32 * 100 / max as u32) as u8
}

/// Egy válasz feldolgozása (a 0-s report ID nélkül, ahogy a hidapi adja). A Pro Wireless második
/// lépése (`ProWireless` szint) külön megy, lásd `read`.
fn parse(proto: Proto, d: &[u8]) -> Option<Reading> {
    let at = |k: usize| d.get(k).copied();
    Some(match proto {
        Proto::Nova7 { discrete } => {
            let status = at(3)?;
            if status == 0x00 {
                return Some(Reading::Offline);
            }
            let raw = at(2)?;
            let charging = matches!(status, 0x01 | 0x02);
            if discrete {
                level(steps(raw, 4), true, charging)
            } else {
                level(raw, false, charging)
            }
        }
        Proto::Nova5 => {
            if at(1)? == 0x02 {
                return Some(Reading::Offline);
            }
            let raw = at(3)?;
            if raw > 100 {
                return None;
            }
            level(raw, false, at(4)? == 0x01)
        }
        Proto::Arctis7Plus => {
            if at(1)? == 0x01 {
                return Some(Reading::Offline);
            }
            level(steps(at(2)?, 4), true, at(3)? == 0x01)
        }
        Proto::GameBuds => {
            let buds = [(at(3)?, at(5)?), (at(4)?, at(6)?)];
            let lowest = buds.iter().filter(|(state, _)| *state == 0x03).map(|(_, l)| *l).min();
            match lowest {
                Some(l) => level(l, false, false),
                None => Reading::Offline, // mindkettő a tokban
            }
        }
        Proto::NovaPro => {
            let status = at(15)?;
            if status == 0x01 {
                return Some(Reading::Offline);
            }
            level(steps(at(6)?, 8), true, status == 0x02)
        }
        Proto::Arctis7 => {
            let raw = at(2)?;
            if raw > 100 {
                return None;
            }
            Reading::Level(raw, false, ChargeState::Unknown)
        }
        Proto::Arctis1 => {
            if at(2)? == 0x01 {
                return Some(Reading::Offline);
            }
            let raw = at(3)?;
            if raw > 100 {
                return None;
            }
            Reading::Level(raw, false, ChargeState::Unknown)
        }
        Proto::Arctis9 => {
            let raw = at(3)?;
            if raw == 0 {
                return None;
            }
            let l = (raw.clamp(0x64, 0x9A) - 0x64) as u32 * 100 / (0x9A - 0x64);
            level(l as u8, false, at(4)? == 0x01)
        }
        Proto::ProWireless => {
            if at(0)? == 0x02 {
                return Some(Reading::Offline);
            }
            return None; // a szint a második kérésből jön
        }
        Proto::Aerox { .. } => {
            let raw = at(1)?;
            let pct = ((raw & 0x7F) as i32 - 1) * 5;
            if !(0..=100).contains(&pct) {
                return None; // az egér alszik vagy ki van kapcsolva
            }
            level(pct as u8, false, raw & 0x80 != 0)
        }
        Proto::Rival { .. } => {
            let raw = at(0)?;
            if raw > 100 {
                return None;
            }
            level(raw, false, at(2)? != 0)
        }
    })
}

/// Írás a kimenő report hosszára kiegészítve, aztán a válasz (0-s report ID nélkül). A várt
/// kezdetű választ részesíti előnyben; ha ilyen nem jön, az első bejövő report számít.
fn transact(dev: &Device, i: &HidInterface, req: &[u8], expect: &[u8]) -> Option<Vec<u8>> {
    let out_len = i.output_len as usize;
    if req.len() > out_len || i.input_len == 0 {
        return None;
    }
    let mut msg = req.to_vec();
    msg.resize(out_len, 0);
    if !dev.write(&msg, 250) {
        return None;
    }
    let deadline = Instant::now() + Duration::from_millis(400);
    let mut first: Option<Vec<u8>> = None;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Some(mut r) = dev.read(i.input_len as usize, left.as_millis().max(1) as u32) else { break };
        if r.first() == Some(&0) {
            r.remove(0);
        }
        if r.starts_with(expect) {
            return Some(r);
        }
        first.get_or_insert(r);
    }
    first
}

/// Egy kollekció kiolvasása. None: nem válaszolt, vagy a válasz nem értelmezhető.
fn read(i: &HidInterface, proto: Proto) -> Option<Reading> {
    let dev = Device::open(&i.path)?;
    let (req, expect) = proto.request();
    let r = transact(&dev, i, req, expect)?;
    let hex: Vec<String> = r.iter().take(24).map(|b| format!("{b:02X}")).collect();
    crate::worker::log(&format!("steelseries {:04X} válasz: {}", i.pid, hex.join(" ")));
    if proto == Proto::ProWireless && parse(proto, &r).is_none() {
        // Második lépés: a szint 0-4 fokozatban; töltési állapotot ez az eszköz nem ad.
        let r = transact(&dev, i, &[0x40, 0xAA], &[])?;
        return Some(Reading::Level(steps(*r.first()?, 4), true, ChargeState::Unknown));
    }
    parse(proto, &r)
}

/// A modellhez tartozó kollekciók (egy interfészen több is lehet; sorban próbáljuk őket).
fn candidates<'a>(all: &'a [HidInterface], m: &Model) -> impl Iterator<Item = &'a HidInterface> {
    let target = m.proto.target();
    let pid = m.pid;
    all.iter().filter(move |i| {
        i.vid == STEELSERIES
            && i.pid == pid
            && i.output_len > 0
            && match target {
                Target::Usage(page, usage) => i.usage_page == page && i.usage == usage,
                Target::Interface(n) => i.interface_number() == Some(n),
            }
    })
}

struct Seen {
    name: &'static str,
    kind: DeviceKind,
    pid: u16,
    /// Az utolsó ismert szint (alvó egérnél ez marad).
    last: Option<(u8, bool, ChargeState)>,
}

#[derive(Default)]
pub struct SteelSeries {
    seen: HashMap<&'static str, Seen>,
    /// Látható vevők / kábeles eszközök száma.
    present: usize,
}

impl Provider for SteelSeries {
    fn scan(&mut self) -> Vec<DeviceState> {
        use crate::worker::log;
        let all = hid::interfaces();
        let mut out: Vec<DeviceState> = Vec::new();
        // Kulcsonként: (látható-e a vevő / kábel, friss olvasás).
        let mut found: HashMap<&'static str, Option<Reading>> = HashMap::new();
        for m in MODELS {
            let mut present = false;
            let mut reading = None;
            for i in candidates(&all, m) {
                present = true;
                reading = read(i, m.proto);
                if reading.is_some() {
                    break;
                }
            }
            if !present {
                continue;
            }
            log(&format!("steelseries {:04X} {}: {reading:?}", m.pid, m.name));
            // Ha egy eszköz két úton is látszik (kábel és vevő), a szintet adó olvasás nyer.
            let slot = found.entry(m.key).or_insert(None);
            if !matches!(slot, Some(Reading::Level(..))) && reading.is_some() {
                *slot = reading;
            }
            let seen = self.seen.entry(m.key).or_insert(Seen { name: m.name, kind: m.kind, pid: m.pid, last: None });
            seen.pid = m.pid;
        }

        self.present = found.len();
        for (key, seen) in self.seen.iter_mut() {
            let state = match found.get(key) {
                None => DeviceState::disconnected(key, seen.name, seen.kind),
                Some(Some(Reading::Offline)) => DeviceState { hw: Some((STEELSERIES, seen.pid)), ..DeviceState::disconnected(key, seen.name, seen.kind) },
                Some(fresh) => {
                    if let Some(Reading::Level(l, approx, charge)) = fresh {
                        seen.last = Some((*l, *approx, *charge));
                    }
                    let (level, level_approx, charge) = match seen.last {
                        Some((l, a, c)) => (Some(l), a, c),
                        None => (None, false, ChargeState::Unknown),
                    };
                    DeviceState {
                        level,
                        level_approx,
                        charge,
                        connected: true,
                        hw: Some((STEELSERIES, seen.pid)),
                        ..DeviceState::disconnected(key, seen.name, seen.kind)
                    }
                }
            };
            out.push(state);
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Amíg egy vevő vagy kábel látható, percenként: a kikapcsolt fejhallgató bekapcsolásáról
    /// nem jön Windows-esemény, a vevő ugyanis végig csatlakoztatva marad.
    fn poll_interval(&self) -> Option<Duration> {
        (self.present > 0).then_some(Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nova7() {
        // Régi firmware: 0-4 fokozat; d[3]: 0 nincs kapcsolat, 1/2 tölt, 3 merül.
        let old = Proto::Nova7 { discrete: true };
        assert_eq!(parse(old, &[0xB0, 0x00, 0x03, 0x03]), Some(Reading::Level(75, true, ChargeState::Discharging)));
        assert_eq!(parse(old, &[0xB0, 0x00, 0x04, 0x01]), Some(Reading::Level(100, true, ChargeState::Full)));
        assert_eq!(parse(old, &[0xB0, 0x00, 0x02, 0x02]), Some(Reading::Level(50, true, ChargeState::Charging)));
        assert_eq!(parse(old, &[0xB0, 0x00, 0x00, 0x00]), Some(Reading::Offline));
        // Gen 2 / új firmware: százalék.
        let new = Proto::Nova7 { discrete: false };
        assert_eq!(parse(new, &[0xB0, 0x00, 0x48, 0x03]), Some(Reading::Level(72, false, ChargeState::Discharging)));
        assert_eq!(parse(new, &[0xB0, 0x00, 0x1E, 0x01]), Some(Reading::Level(30, false, ChargeState::Charging)));
        assert_eq!(parse(new, &[0xB0, 0x00]), None);
    }

    #[test]
    fn other_headsets() {
        assert_eq!(parse(Proto::Nova5, &[0xB0, 0x02, 0, 0, 0]), Some(Reading::Offline));
        assert_eq!(parse(Proto::Nova5, &[0xB0, 0x03, 0, 0x50, 0x01]), Some(Reading::Level(80, false, ChargeState::Charging)));
        assert_eq!(parse(Proto::Arctis7Plus, &[0xB0, 0x01, 0, 0]), Some(Reading::Offline));
        assert_eq!(parse(Proto::Arctis7Plus, &[0xB0, 0x03, 0x01, 0x00]), Some(Reading::Level(25, true, ChargeState::Discharging)));
        assert_eq!(parse(Proto::GameBuds, &[0xB0, 0, 0, 0x03, 0x03, 60, 40]), Some(Reading::Level(40, false, ChargeState::Discharging)));
        assert_eq!(parse(Proto::GameBuds, &[0xB0, 0, 0, 0x03, 0x02, 60, 40]), Some(Reading::Level(60, false, ChargeState::Discharging)));
        assert_eq!(parse(Proto::GameBuds, &[0xB0, 0, 0, 0x02, 0x02, 60, 40]), Some(Reading::Offline));
        let mut pro = [0u8; 16];
        pro[..2].copy_from_slice(&[0x06, 0xB0]);
        pro[6] = 6;
        pro[15] = 0x02;
        assert_eq!(parse(Proto::NovaPro, &pro), Some(Reading::Level(75, true, ChargeState::Charging)));
        pro[15] = 0x01;
        assert_eq!(parse(Proto::NovaPro, &pro), Some(Reading::Offline));
        assert_eq!(parse(Proto::Arctis7, &[0x06, 0x18, 55]), Some(Reading::Level(55, false, ChargeState::Unknown)));
        assert_eq!(parse(Proto::Arctis1, &[0x06, 0x12, 0x01, 0]), Some(Reading::Offline));
        assert_eq!(parse(Proto::Arctis1, &[0x06, 0x12, 0x03, 90]), Some(Reading::Level(90, false, ChargeState::Unknown)));
        assert_eq!(parse(Proto::Arctis9, &[0, 0, 0, 0x9A, 0x01]), Some(Reading::Level(100, false, ChargeState::Full)));
        assert_eq!(parse(Proto::Arctis9, &[0, 0, 0, 0x64, 0x00]), Some(Reading::Level(0, false, ChargeState::Discharging)));
    }

    #[test]
    fn mice() {
        let a = Proto::Aerox { wireless: true };
        assert_eq!(parse(a, &[0xD2, 21]), Some(Reading::Level(100, false, ChargeState::Discharging)));
        assert_eq!(parse(a, &[0xD2, 0x80 | 11]), Some(Reading::Level(50, false, ChargeState::Charging)));
        assert_eq!(parse(a, &[0xD2, 0]), None); // alszik
        let r = Proto::Rival { interface: 3 };
        assert_eq!(parse(r, &[64, 0, 0]), Some(Reading::Level(64, false, ChargeState::Discharging)));
        assert_eq!(parse(r, &[64, 0, 1]), Some(Reading::Level(64, false, ChargeState::Charging)));
    }

    #[test]
    fn keys_share_kind_and_name() {
        for a in MODELS {
            for b in MODELS.iter().filter(|b| b.key == a.key) {
                assert_eq!((a.name, a.kind), (b.name, b.kind), "{}", a.key);
            }
            assert_eq!(MODELS.iter().filter(|b| b.pid == a.pid).count(), 1, "PID {:04X} kétszer", a.pid);
        }
    }

    /// A csatlakoztatott SteelSeries eszközök (`cargo test steelseries_present -- --ignored --nocapture`).
    #[test]
    #[ignore = "a csatlakoztatott hardvertől függ"]
    fn steelseries_present() {
        for i in hid::interfaces().iter().filter(|i| i.vid == STEELSERIES) {
            println!(
                "{:04X}:{:04X} {:?} mi={:?} 0x{:04X}/0x{:04X} in {} out {} feature {}",
                i.vid, i.pid, i.product, i.interface_number(), i.usage_page, i.usage, i.input_len, i.output_len, i.feature_len
            );
        }
        for d in SteelSeries::default().scan() {
            println!("{}", d.tooltip());
        }
    }
}
