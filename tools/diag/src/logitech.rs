//! Logitech eszközök: HID++ lekérdezés a NapCat logitech_hidpp providerének lépései szerint, de
//! minden kérést és nyers választ kiír, hogy egy nem működő eszköznél látszódjon, hol akad el.
//!
//! Csak olvasó kéréseket küld (protokollverzió, funkciólista, név, típus, akku, a vevő
//! kapcsolat-regisztere), és csak a HID++ kollekciókba (0xFF00/0x0001 rövid, 0xFF00/0x0002
//! hosszú). A többi gyártói csatornát (pl. a G435 vevőjének 64 bájtos csatornáját) csak figyeli.

use std::fmt::Write;
use std::time::{Duration, Instant};

use crate::io::{hex, Dev};
use crate::HidInfo;

const LOGITECH: u16 = 0x046D;
const SHORT: u8 = 0x10;
const LONG: u8 = 0x11;
const SWID: u8 = 0x0A;
const DIRECT: u8 = 0xFF;

macro_rules! out {
    ($s:expr, $($arg:tt)*) => {{
        let _ = writeln!($s, $($arg)*);
    }};
}

fn feature_name(id: u16) -> &'static str {
    match id {
        0x0000 => "ROOT",
        0x0001 => "FEATURE_SET",
        0x0002 => "FEATURE_INFO",
        0x0003 => "DEVICE_INFO",
        0x0005 => "DEVICE_NAME",
        0x0007 => "DEVICE_FRIENDLY_NAME",
        0x0020 => "CONFIG_CHANGE",
        0x00C2 => "DFU_CONTROL",
        0x00D0 => "DFU",
        0x1000 => "BATTERY_STATUS",
        0x1001 => "BATTERY_VOLTAGE",
        0x1004 => "UNIFIED_BATTERY",
        0x1814 => "CHANGE_HOST",
        0x1815 => "HOSTS_INFO",
        0x1B04 => "REPROG_CONTROLS_V4",
        0x1D4B => "WIRELESS_DEVICE_STATUS",
        0x1F20 => "ADC_MEASUREMENT",
        0x2201 => "ADJUSTABLE_DPI",
        0x2250 => "ANALYSIS_MODE",
        0x4521 => "KEYBOARD_DISABLE_KEYS",
        0x8060 => "REPORT_RATE",
        0x8061 => "EXTENDED_REPORT_RATE",
        0x8070 => "COLOR_LED_EFFECTS",
        0x8071 => "RGB_EFFECTS",
        0x8100 => "ONBOARD_PROFILES",
        0x8300 => "SIDETONE",
        0x8310 => "EQUALIZER",
        _ => "",
    }
}

/// Egy fizikai eszköz kollekciói (interfészek és kollekciók): VID/PID szerint; két egyforma
/// vevő egyszerre ritka, és akkor is ugyanazt kell kérdezni rajtuk.
fn group_key(d: &HidInfo) -> (u16, u16) {
    (d.vid, d.pid)
}

struct Hidpp<'a> {
    long: &'a Dev,
    long_len: usize,
    short: Option<&'a Dev>,
    s: &'a mut String,
}

impl Hidpp<'_> {
    /// HID++ 2.0 kérés a hosszú csatornán; minden kapott jelentést kiír, a választ visszaadja.
    fn request(&mut self, index: u8, feature: u8, function: u8, params: &[u8]) -> Result<[u8; 16], String> {
        let mut msg = vec![0u8; self.long_len.max(20)];
        msg[0] = LONG;
        msg[1] = index;
        msg[2] = feature;
        msg[3] = (function << 4) | SWID;
        msg[4..4 + params.len()].copy_from_slice(params);
        if !self.long.write(&msg, 200) {
            return Err("az írás nem sikerült".into());
        }
        let deadline = Instant::now() + Duration::from_millis(400);
        let mut buf = vec![0u8; msg.len()];
        while Instant::now() < deadline {
            if let Some(n) = self.long.read(&mut buf, 20) {
                let r = &buf[..n];
                if n >= 20 && r[0] == LONG && r[1] == index && r[2] == feature && r[3] == msg[3] {
                    let mut p = [0u8; 16];
                    p.copy_from_slice(&r[4..20]);
                    return Ok(p);
                }
                if n >= 20 && r[0] == LONG && r[1] == index && r[2] == 0xFF && r[3] == feature && r[4] == msg[3] {
                    return Err(format!("HID++ 2.0 hiba 0x{:02X} ({})", r[5], error2(r[5])));
                }
                out!(self.s, "      (más jelentés: {})", hex(r));
            }
            if let Some(sh) = self.short {
                let mut b = [0u8; 7];
                if let Some(n) = sh.read(&mut b, 20) {
                    let r = &b[..n];
                    if n >= 6 && r[0] == SHORT && r[1] == index && r[2] == 0x8F && r[3] == feature {
                        return Err(format!("HID++ 1.0 hiba 0x{:02X} ({}) – rövid csatornán: {}", r[5], error1(r[5]), hex(r)));
                    }
                    out!(self.s, "      (rövid jelentés: {})", hex(r));
                }
            }
        }
        Err("nincs válasz (időtúllépés)".into())
    }
}

fn error1(c: u8) -> &'static str {
    match c {
        0x01 => "invalid SubID / command",
        0x02 => "invalid address",
        0x03 => "invalid value",
        0x04 => "connection request failed",
        0x05 => "too many devices",
        0x06 => "already exists",
        0x07 => "busy",
        0x08 => "unknown device",
        0x09 => "resource error / nincs csatlakoztatva",
        0x0A => "request unavailable",
        0x0B => "invalid param value",
        0x0C => "wrong pin code",
        _ => "?",
    }
}

fn error2(c: u8) -> &'static str {
    match c {
        0x01 => "unknown",
        0x02 => "invalid argument",
        0x03 => "out of range",
        0x04 => "hardware error",
        0x05 => "logitech internal",
        0x06 => "invalid feature index",
        0x07 => "invalid function",
        0x08 => "busy",
        0x09 => "unsupported",
        _ => "?",
    }
}

/// Egy eszközhely (közvetlen vagy vevőn 1-6) végigkérdezése; a HID++ főverzió, 0 ha nem válaszolt.
fn probe(h: &mut Hidpp, index: u8) -> u8 {
    let label = if index == DIRECT { "közvetlen (0xFF)".to_string() } else { format!("vevő {index}. hely") };
    out!(h.s, "  --- {label} ---");
    let ver = match h.request(index, 0x00, 1, &[0, 0, 0x5A]) {
        Ok(r) => r,
        Err(e) => {
            out!(h.s, "    protokollverzió: {e}");
            return 0;
        }
    };
    out!(h.s, "    protokoll: HID++ {}.{} (ping {:02X})", ver[0], ver[1], ver[2]);
    if ver[0] < 2 {
        return ver[0].max(1);
    }
    // Funkciólista a FEATURE_SET-ből.
    let mut features: Vec<(u8, u16)> = Vec::new();
    match h.request(index, 0x00, 0, &0x0001u16.to_be_bytes()) {
        Ok(r) if r[0] != 0 => {
            let fs = r[0];
            match h.request(index, fs, 0, &[]) {
                Ok(c) => {
                    for i in 1..=c[0] {
                        match h.request(index, fs, 1, &[i]) {
                            Ok(f) => features.push((i, u16::from_be_bytes([f[0], f[1]]))),
                            Err(e) => out!(h.s, "    funkció {i}: {e}"),
                        }
                    }
                }
                Err(e) => out!(h.s, "    FEATURE_SET.count: {e}"),
            }
        }
        Ok(_) => out!(h.s, "    nincs FEATURE_SET"),
        Err(e) => out!(h.s, "    FEATURE_SET keresése: {e}"),
    }
    features.insert(0, (0, 0x0000));
    out!(h.s, "    funkciók ({}):", features.len());
    for (i, f) in &features {
        out!(h.s, "      [{i:>2}] 0x{f:04X} {}", feature_name(*f));
    }
    let idx = |id: u16| features.iter().find(|f| f.1 == id).map(|f| f.0);

    if let Some(fi) = idx(0x0005) {
        let mut name = Vec::new();
        if let Ok(r) = h.request(index, fi, 0, &[]) {
            let len = r[0] as usize;
            while name.len() < len.min(64) {
                match h.request(index, fi, 1, &[name.len() as u8]) {
                    Ok(c) => {
                        let before = name.len();
                        name.extend(c.iter().take(len - name.len()).take_while(|&&b| b != 0));
                        if name.len() == before {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        out!(h.s, "    név: \"{}\"", String::from_utf8_lossy(&name).trim());
        match h.request(index, fi, 2, &[]) {
            Ok(r) => out!(h.s, "    típus: {} ({})", r[0], match r[0] {
                0 => "billentyűzet",
                1 => "távirányító",
                2 => "numpad",
                3 => "egér",
                4 => "touchpad",
                5 => "trackball",
                6 => "presenter",
                7 => "vevő",
                8 => "headset",
                _ => "?",
            }),
            Err(e) => out!(h.s, "    típus: {e}"),
        }
    }
    if let Some(fi) = idx(0x0003) {
        match h.request(index, fi, 0, &[]) {
            Ok(r) => out!(h.s, "    DEVICE_INFO: {}  (unitId {})", hex(&r), hex(&r[1..5])),
            Err(e) => out!(h.s, "    DEVICE_INFO: {e}"),
        }
    }
    if let Some(fi) = idx(0x1D4B) {
        match h.request(index, fi, 0, &[]) {
            Ok(r) => out!(h.s, "    WIRELESS_DEVICE_STATUS: {}", hex(&r)),
            Err(e) => out!(h.s, "    WIRELESS_DEVICE_STATUS: {e}"),
        }
    }

    let mut battery = false;
    if let Some(fi) = idx(0x1004) {
        battery = true;
        match h.request(index, fi, 0, &[]) {
            Ok(r) => out!(h.s, "    UNIFIED_BATTERY.getCapabilities: {}", hex(&r)),
            Err(e) => out!(h.s, "    UNIFIED_BATTERY.getCapabilities: {e}"),
        }
        match h.request(index, fi, 1, &[]) {
            Ok(r) => out!(h.s, "    UNIFIED_BATTERY.getStatus: {}  -> {}%, szintjelzők 0x{:02X}, töltés {} ({}), külső táp {}",
                hex(&r), r[0], r[1], r[2], match r[2] { 0 => "merül", 1 => "tölt", 2 => "lassan tölt", 3 => "teli", 4 => "hiba", _ => "?" }, r[3]),
            Err(e) => out!(h.s, "    UNIFIED_BATTERY.getStatus: {e}"),
        }
    }
    if let Some(fi) = idx(0x1000) {
        battery = true;
        match h.request(index, fi, 0, &[]) {
            Ok(r) => out!(h.s, "    BATTERY_STATUS: {}  -> {}%, következő {}%, állapot {} ({})", hex(&r), r[0], r[1], r[2],
                match r[2] { 0 => "merül", 1 => "tölt", 2 => "töltés vége felé", 3 => "teli", 4 => "lassan tölt", 5 => "érvénytelen akku", 6 => "hőmérséklet-hiba", _ => "töltési hiba" }),
            Err(e) => out!(h.s, "    BATTERY_STATUS: {e}"),
        }
    }
    if let Some(fi) = idx(0x1001) {
        battery = true;
        match h.request(index, fi, 0, &[]) {
            Ok(r) => out!(h.s, "    BATTERY_VOLTAGE: {}  -> {} mV, jelzők 0x{:02X}", hex(&r), u16::from_be_bytes([r[0], r[1]]), r[2]),
            Err(e) => out!(h.s, "    BATTERY_VOLTAGE: {e}"),
        }
    }
    if let Some(fi) = idx(0x1F20) {
        battery = true;
        match h.request(index, fi, 0, &[]) {
            Ok(r) => out!(h.s, "    ADC_MEASUREMENT: {}  -> {} mV, jelzők 0x{:02X}", hex(&r), u16::from_be_bytes([r[0], r[1]]), r[2]),
            Err(e) => out!(h.s, "    ADC_MEASUREMENT: {e}"),
        }
    }
    if !battery {
        out!(h.s, "    nincs ismert akku funkció (0x1000 / 0x1001 / 0x1004 / 0x1F20)");
    }
    ver[0]
}

/// A vevő HID++ 1.0 regisztere: 0x02 = kapcsolat állapota (csatlakoztatott eszközök száma).
fn receiver_registers(short: &Dev, s: &mut String) {
    let req = [SHORT, DIRECT, 0x81, 0x02, 0, 0, 0];
    if !short.write(&req, 200) {
        out!(s, "  vevő 0x02 regiszter: az írás nem sikerült");
        return;
    }
    let deadline = Instant::now() + Duration::from_millis(400);
    let mut b = [0u8; 7];
    while Instant::now() < deadline {
        if let Some(n) = short.read(&mut b, 50) {
            let r = &b[..n];
            if r[1] == DIRECT && r[2] == 0x81 && r[3] == 0x02 {
                out!(s, "  vevő 0x02 regiszter: {}  -> {} csatlakoztatott eszköz", hex(r), r[5]);
                return;
            }
            if r[1] == DIRECT && r[2] == 0x8F && r[3] == 0x81 {
                out!(s, "  vevő 0x02 regiszter: hiba 0x{:02X} ({})", r[5], error1(r[5]));
                return;
            }
            out!(s, "    (rövid jelentés: {})", hex(r));
        }
    }
    out!(s, "  vevő 0x02 regiszter: nincs válasz");
}

/// Passzív figyelés a nem HID++ gyártói csatornákon.
fn listen(d: &HidInfo, s: &mut String, secs: u64) {
    let Some(dev) = Dev::open(&d.path) else {
        out!(s, "  nem nyitható meg");
        return;
    };
    let start = Instant::now();
    let mut buf = vec![0u8; d.in_len as usize];
    let mut count = 0;
    while start.elapsed() < Duration::from_secs(secs) && count < 20 {
        if let Some(n) = dev.read(&mut buf, 250) {
            count += 1;
            out!(s, "  {:>5.2}s {}", start.elapsed().as_secs_f32(), hex(&buf[..n.min(64)]));
        }
    }
    if count == 0 {
        out!(s, "  {secs} mp alatt nem jött semmi");
    }
}

pub unsafe fn run() -> String {
    let mut s = String::new();
    let all = crate::all_hid_info();
    let mine: Vec<&HidInfo> = all.iter().filter(|d| d.vid == LOGITECH).collect();
    out!(s, "=== Logitech HID kollekciók ===");
    if mine.is_empty() {
        out!(s, "Nincs Logitech eszköz (VID 046D). Dugd be a vevőt vagy a kábelt, és futtasd újra.");
        return s;
    }
    let mut groups: Vec<((u16, u16), Vec<&HidInfo>)> = Vec::new();
    for d in &mine {
        let k = group_key(d);
        match groups.iter_mut().find(|g| g.0 == k) {
            Some(g) => g.1.push(d),
            None => groups.push((k, vec![d])),
        }
    }
    for d in &mine {
        out!(s, "{:04X}:{:04X} \"{}\" 0x{:04X}/0x{:04X} in {} out {} feature {}", d.vid, d.pid, d.product, d.page, d.usage, d.in_len, d.out_len, d.feat_len);
    }
    for (_, cols) in &groups {
        let d0 = cols[0];
        out!(s, "");
        out!(s, "=== {:04X}:{:04X} \"{}\" ===", d0.vid, d0.pid, d0.product);
        let long = cols.iter().find(|d| d.page == 0xFF00 && d.usage == 0x0002 && d.in_len >= 20 && d.out_len >= 20);
        let short = cols.iter().find(|d| d.page == 0xFF00 && d.usage == 0x0001 && d.in_len == 7 && d.out_len == 7);
        let others: Vec<_> = cols.iter().filter(|d| d.page >= 0xFF00 && d.in_len > 0).filter(|d| !matches!((d.page, d.usage), (0xFF00, 1 | 2)) || (d.in_len != 7 && d.in_len != 20)).collect();
        match long {
            None => out!(s, "  nincs HID++ hosszú kollekció (0xFF00/0x0002, 20 bájt): a NapCat itt nem kérdez"),
            Some(l) => {
                let Some(ldev) = Dev::open(&l.path) else {
                    out!(s, "  a HID++ kollekció nem nyitható meg (más program fogja?)");
                    continue;
                };
                let sdev = short.and_then(|sh| Dev::open(&sh.path));
                if let Some(sh) = &sdev {
                    receiver_registers(sh, &mut s);
                }
                let mut h = Hidpp { long: &ldev, long_len: l.out_len as usize, short: sdev.as_ref(), s: &mut s };
                // Ahogy a provider: előbb közvetlen eszköz, ha az nem HID++ 2.0, a vevő helyei.
                if probe(&mut h, DIRECT) < 2 {
                    for index in 1..=6 {
                        probe(&mut h, index);
                    }
                }
            }
        }
        for o in others {
            out!(s, "");
            out!(s, "  figyelés 0x{:04X}/0x{:04X} (in {} bájt), 5 mp, semmit nem küld:", o.page, o.usage, o.in_len);
            listen(o, &mut s, 5);
        }
    }
    s
}
