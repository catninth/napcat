//! VIA Raw HID lekérdezés (usage page 0xFF60, usage 0x61).
//!
//! Csak olvasó parancsokat küld, semmit nem ír a billentyűzet memóriájába:
//!   0x01 id_get_protocol_version
//!   0x02 id_get_keyboard_value: 0x01 uptime, 0x04 firmware version
//! Ismeretlen parancsra a QMK 0xFF-fel (id_unhandled) válaszol.

use crate::io::{hex, Dev};

const QUERIES: &[(&str, &[u8])] = &[
    ("protokollverzió", &[0x01]),
    ("uptime", &[0x02, 0x01]),
    ("firmware verzió", &[0x02, 0x04]),
    // id_custom_get_value, csatorna 0 (id_custom_channel), érték 1: az RK R75 Pro firmware-ében az
    // alvási idő (wls.c). Ha erre jön értelmes válasz, a kérés a billentyűzet saját kódjáig eljut.
    ("alvási idő (RK)", &[0x08, 0x00, 0x01]),
    // A NapCat-hez módosított R75 Pro firmware akkuértéke: [08 00 05 szint% állapot]; gyári firmware-en FF.
    ("akku (RK NapCat)", &[0x08, 0x00, 0x05]),
];

pub unsafe fn run() {
    let devs: Vec<_> = crate::all_hid_info()
        .into_iter()
        .filter(|d| d.page == 0xFF60 && d.usage == 0x61)
        .collect();
    if devs.is_empty() {
        println!("Nincs VIA Raw HID interfész (0xFF60/0x61).");
        return;
    }
    for d in devs {
        println!("=== {:04X}:{:04X} \"{}\"  in/out {}/{}", d.vid, d.pid, d.product, d.in_len, d.out_len);
        let Some(dev) = Dev::open(&d.path) else {
            println!("  nem nyitható meg írás-olvasásra");
            continue;
        };
        for (name, cmd) in QUERIES {
            match transact(&dev, cmd, d.out_len as usize, d.in_len as usize) {
                Some(resp) => {
                    println!("  {name:<16} -> {}", hex(&resp));
                    if resp.first() == Some(&0xFF) {
                        println!("  {:<16}    (id_unhandled: a firmware nem ismeri ezt a parancsot)", "");
                    }
                }
                None => println!("  {name:<16} -> nincs válasz"),
            }
        }
    }
}

/// Elküld egy 32 bájtos VIA parancsot, és megvárja azt a választ, amelynek első bájtja egyezik
/// (vagy 0xFF). A közben érkező egyéb reportokat kiírja, mert azok is érdekesek lehetnek.
fn transact(dev: &Dev, cmd: &[u8], out_len: usize, in_len: usize) -> Option<Vec<u8>> {
    let mut out = vec![0u8; out_len];
    out[1..1 + cmd.len()].copy_from_slice(cmd); // out[0] = report ID 0
    if !dev.write(&out, 1000) {
        return None;
    }
    let mut buf = vec![0u8; in_len];
    for _ in 0..8 {
        let n = dev.read(&mut buf, 1500)?;
        let resp = &buf[1..n]; // a report ID nélkül
        if resp.first() == Some(&cmd[0]) || resp.first() == Some(&0xFF) {
            return Some(trim(resp));
        }
        println!("  (egyéb report)    -> {}", hex(&trim(resp)));
    }
    None
}

/// A záró nullák levágása a könnyebb olvashatóságért (legalább 4 bájt marad).
fn trim(b: &[u8]) -> Vec<u8> {
    let end = b.iter().rposition(|&x| x != 0).map_or(0, |i| i + 1).max(4.min(b.len()));
    b[..end].to_vec()
}
