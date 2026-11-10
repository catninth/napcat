//! MCHOSE egerek (A7 V2 család) akkulekérdezése, az MCHOSE webes driver protokollja szerint.
//!
//! 0x11-es feature report, a report ID utáni bájtok invertálva (XOR 0xFF). Csak az eszközadat
//! (0x06) olvasó parancsot küldi; az egér "e5 06" input reporttal jelez, utána a válasz feature
//! reportként olvasható ki: [0x11, parancs, vid, pid, firmware (4), kapcsolat, szint %, töltés].
//! A teljes kimenet szövegként jön vissza, hogy fájlba is menthető legyen.

use std::fmt::Write;
use std::time::{Duration, Instant};

use crate::io::{hex, Dev};

const VIDS: [u16; 2] = [0x3837, 0x5253];
const REPORT: u8 = 0x11;
const GET_DEVICE_INFO: u8 = 0x06;
const REPORT_LEN: usize = 21;

macro_rules! out {
    ($s:expr, $($arg:tt)*) => {{
        let _ = writeln!($s, $($arg)*);
    }};
}

pub unsafe fn run() -> String {
    let mut s = String::new();
    let all = crate::all_hid_info();
    let mine: Vec<_> = all.iter().filter(|d| VIDS.contains(&d.vid)).collect();
    out!(s, "=== MCHOSE HID kollekciók ===");
    if mine.is_empty() {
        out!(s, "Nincs MCHOSE eszköz (VID 3837 / 5253). Dugd be az egeret vagy a vevőt, és futtasd újra.");
        out!(s, "");
        out!(s, "Az összes HID eszköz:");
        for d in &all {
            out!(s, "  {:04X}:{:04X} \"{}\" 0x{:04X}/0x{:04X}", d.vid, d.pid, d.product, d.page, d.usage);
        }
        return s;
    }
    for d in &mine {
        out!(s, "{:04X}:{:04X} \"{}\" 0x{:04X}/0x{:04X} in {} out {} feature {}", d.vid, d.pid, d.product, d.page, d.usage, d.in_len, d.out_len, d.feat_len);
    }
    let targets: Vec<_> = mine.iter().filter(|d| d.page >= 0xFF00 && d.feat_len as usize >= REPORT_LEN).collect();
    if targets.is_empty() {
        out!(s, "Nincs gyártói kollekció legalább {REPORT_LEN} bájtos feature reporttal.");
        return s;
    }
    for d in targets {
        out!(s, "");
        out!(s, "=== {:04X}:{:04X} 0x{:04X}/0x{:04X} (feature {}) ===", d.vid, d.pid, d.page, d.usage, d.feat_len);
        let Some(dev) = Dev::open(&d.path) else {
            out!(s, "  nem nyitható meg");
            continue;
        };
        // Az eszköz többi gyártói kollekcióján is figyeljük a jelzést.
        let listeners: Vec<_> = mine
            .iter()
            .filter(|o| o.vid == d.vid && o.pid == d.pid && o.page >= 0xFF00 && o.in_len > 0)
            .filter_map(|o| Some((o.page, o.usage, o.in_len as usize, Dev::open(&o.path)?)))
            .collect();
        let len = (d.feat_len as usize).max(REPORT_LEN);
        let mut req = vec![0xFFu8; len];
        req[0] = REPORT;
        req[1] = !GET_DEVICE_INFO;
        out!(s, "  kérés: {}", hex(&req[..REPORT_LEN]));
        // Alvó egérnél 0% jön: legfeljebb 15 próbálkozás, közben mozgatni kell az egeret.
        let mut done = false;
        for attempt in 1..=15 {
            if !dev.set_feature(&req) {
                out!(s, "  {attempt}. SetFeature nem sikerült");
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            let deadline = Instant::now() + Duration::from_millis(400);
            while Instant::now() < deadline {
                for (page, usage, in_len, o) in &listeners {
                    let mut buf = vec![0u8; *in_len];
                    if let Some(n) = o.read(&mut buf, 10) {
                        let inv: Vec<u8> = buf[..n].iter().map(|b| !b).collect();
                        out!(s, "  input 0x{page:04X}/0x{usage:04X}: {}  (invertálva {})", hex(&buf[..n]), hex(&inv));
                    }
                }
            }
            match dev.get_feature(REPORT, len) {
                None => out!(s, "  {attempt}. GetFeature nem sikerült"),
                Some(r) => {
                    let inv: Vec<u8> = r.iter().skip(1).map(|b| !b).collect();
                    out!(s, "  {attempt}. válasz: {}", hex(&r[..r.len().min(32)]));
                    out!(s, "     invertálva: {}", hex(&inv[..inv.len().min(31)]));
                    if inv.len() >= 12 && inv[0] == GET_DEVICE_INFO {
                        let p = &inv[1..];
                        let vid = u16::from_le_bytes([p[0], p[1]]);
                        let pid = u16::from_le_bytes([p[2], p[3]]);
                        let mode = p[8] & 0x07;
                        let status = (p[8] >> 3) & 1;
                        out!(s, "     vid {vid:04X} pid {pid:04X} firmware {} mód {mode} ({}) állapot {status} szint {}% töltés {}",
                            hex(&p[4..8]), if mode == 0 { "kábel" } else { "vezeték nélküli" }, p[9], p[10]);
                        if pid != 0 && p[9] > 0 {
                            done = true;
                            break;
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        out!(s, "  {}", if done { "OK: megvan a szint" } else { "nem jött érvényes szint (alszik az egér, vagy más a protokoll)" });
    }
    s
}
