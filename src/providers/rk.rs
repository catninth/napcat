//! Royal Kludge R75 Pro: akkuszint a VIA Raw HID csatornán (0xFF60/0x61), kábelen és a 2.4G
//! dongle-n át is (a dongle a VIA kéréseket a billentyűzetig továbbítja).
//!
//! A gyári firmware nem adja ki az akkuszintet; a NapCat-hez módosított firmware egy saját VIA
//! értéket ad hozzá: id_custom_get_value (0x08), id_custom_channel (0x00), id_battery (0x05) ->
//! [08 00 05 szint% állapot], az állapot 0 merül / 1 tölt / 2 tele. Ez a kérés a billentyűzet
//! alvás-időzítőjét nem nullázza. Gyári firmware-en a válasz első bájtja 0xFF (id_unhandled):
//! ilyenkor csak a kapcsolat látszik, ahogy korábban (kábel = tölt, dongle = szint ismeretlen).

use std::time::{Duration, Instant};

use super::hid::{self, Device, HidInterface};
use super::Provider;
use crate::model::{ChargeState, DeviceKind, DeviceState};

const RK: u16 = 0x342D;
const R75_PRO: u16 = 0xE487;
const ID: &str = "rk-r75pro";
const NAME: &str = "Royal Kludge R75 Pro";

const BATTERY_QUERY: [u8; 3] = [0x08, 0x00, 0x05];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Link {
    Cable,
    Dongle,
}

/// A válasz (report ID nélkül): Some(szint, töltés), vagy None, ha a firmware nem ismeri a kérést.
fn parse(r: &[u8]) -> Option<(u8, ChargeState)> {
    if r.len() < 5 || r[..3] != BATTERY_QUERY {
        return None;
    }
    let level = r[3];
    if level > 100 {
        return None;
    }
    let charge = match r[4] {
        0 => ChargeState::Discharging,
        1 => ChargeState::Charging,
        2 => ChargeState::Full,
        _ => ChargeState::Unknown,
    };
    Some((if charge == ChargeState::Full { 100 } else { level }, charge))
}

fn read(i: &HidInterface) -> Option<(u8, ChargeState)> {
    let dev = Device::open(&i.path)?;
    let mut msg = vec![0u8; (i.output_len as usize).max(33)];
    msg[1..4].copy_from_slice(&BATTERY_QUERY);
    if !dev.write(&msg, 300) {
        return None;
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let mut r = dev.read(i.input_len as usize, left.as_millis().max(1) as u32)?;
        if r.first() == Some(&0) {
            r.remove(0);
        }
        if r.first() == Some(&0xFF) {
            return None; // gyári firmware: nincs ilyen érték
        }
        if r.starts_with(&BATTERY_QUERY) {
            let parsed = parse(&r);
            crate::worker::log(&format!("rk: akku {parsed:?}"));
            return parsed;
        }
    }
    None
}

/// A firmware a modul szintjét induláskor 100-ra állítja, és csak a modul első válasza (≈3 mp)
/// után lesz valódi: ébredés után ezért egy merülő 100% lehet helyőrző. Ha előtte nem 100 volt,
/// csak egy későbbi, ugyanilyen olvasás után fogadjuk el.
fn needs_confirm(fresh: (u8, ChargeState), last: Option<(u8, ChargeState)>) -> bool {
    fresh == (100, ChargeState::Discharging) && last.map_or(true, |(l, _)| l != 100)
}

#[derive(Default)]
pub struct Rk {
    seen: bool,
    present: bool,
    /// Az utolsó ismert szint (alvó billentyűzetnél ez marad).
    last: Option<(u8, ChargeState)>,
    /// Megerősítésre váró olvasás (lásd `needs_confirm`).
    pending: Option<(u8, ChargeState)>,
}

impl Provider for Rk {
    fn scan(&mut self) -> Vec<DeviceState> {
        let all = hid::interfaces();
        let link = all.iter().filter(|i| i.vid == RK && i.pid == R75_PRO).find_map(|i| match i.product.as_str() {
            "R75Pro" => Some(Link::Cable),
            "RK2.4G Dongle" => Some(Link::Dongle),
            _ => None,
        });
        self.present = link.is_some();
        let Some(link) = link else {
            return if self.seen { vec![DeviceState::disconnected(ID, NAME, DeviceKind::Keyboard)] } else { Vec::new() };
        };
        self.seen = true;
        let via = all.iter().find(|i| i.vid == RK && i.pid == R75_PRO && i.usage_page == 0xFF60 && i.usage == 0x61);
        if let Some(fresh) = via.and_then(read) {
            if needs_confirm(fresh, self.last) && self.pending != Some(fresh) {
                self.pending = Some(fresh);
            } else {
                self.last = Some(fresh);
                self.pending = None;
            }
        }
        let (level, charge) = match (self.last, link) {
            (Some((l, c)), _) => (Some(l), c),
            // Módosítatlan firmware: csak a kapcsolat ismert.
            (None, Link::Cable) => (None, ChargeState::Charging),
            (None, Link::Dongle) => (None, ChargeState::Unknown),
        };
        vec![DeviceState {
            level,
            charge,
            connected: true,
            hw: Some((RK, R75_PRO)),
            ..DeviceState::disconnected(ID, NAME, DeviceKind::Keyboard)
        }]
    }

    /// Ritkán: a billentyűzet a kérésre felébredhet, és alvás közben nem merül a szint gyorsan.
    /// Megerősítésre váró olvasásnál hamar újra.
    fn poll_interval(&self) -> Option<Duration> {
        self.present.then_some(Duration::from_secs(if self.pending.is_some() { 10 } else { 300 }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_reply() {
        assert_eq!(parse(&[0x08, 0x00, 0x05, 72, 0]), Some((72, ChargeState::Discharging)));
        assert_eq!(parse(&[0x08, 0x00, 0x05, 40, 1]), Some((40, ChargeState::Charging)));
        assert_eq!(parse(&[0x08, 0x00, 0x05, 97, 2]), Some((100, ChargeState::Full)));
        assert_eq!(parse(&[0xFF, 0x00, 0x05, 0, 0]), None); // gyári firmware
        assert_eq!(parse(&[0x08, 0x00, 0x01, 7]), None); // más érték válasza
    }

    #[test]
    fn wake_placeholder() {
        use ChargeState::*;
        assert!(needs_confirm((100, Discharging), Some((97, Discharging)))); // ébredés utáni helyőrző
        assert!(needs_confirm((100, Discharging), None));
        assert!(!needs_confirm((100, Discharging), Some((100, Full)))); // lehúzott, feltöltött billentyűzet
        assert!(!needs_confirm((96, Discharging), Some((97, Discharging))));
        assert!(!needs_confirm((100, Full), Some((97, Charging))));
    }

    /// A billentyűzeten futó kiosztás (VIA id_dynamic_keymap_get_keycode, csak olvas): hol van a
    /// QK_BOOT (0x7C00) és a MO() rétegváltók (0x5220 + réteg).
    /// `cargo test rk_keymap -- --ignored --nocapture`
    #[test]
    #[ignore = "a csatlakoztatott hardvertől függ"]
    fn rk_keymap() {
        let all = hid::interfaces();
        let i = all.iter().find(|i| i.vid == RK && i.pid == R75_PRO && i.usage_page == 0xFF60).expect("VIA interfész");
        println!("kapcsolat: {}", i.product);
        let dev = Device::open(&i.path).expect("megnyitás");
        let ask = |cmd: &[u8]| -> Option<Vec<u8>> {
            let mut msg = vec![0u8; i.output_len as usize];
            msg[1..1 + cmd.len()].copy_from_slice(cmd);
            dev.write(&msg, 300).then_some(())?;
            for _ in 0..5 {
                let r = dev.read(i.input_len as usize, 500)?;
                if r[1] == cmd[0] || r[1] == 0xFF {
                    return Some(r[1..].to_vec());
                }
            }
            None
        };
        let layers = ask(&[0x11]).map(|r| r[1]).unwrap_or(5); // id_dynamic_keymap_get_layer_count
        println!("rétegek: {layers}");
        for layer in 0..layers {
            for row in 0..6u8 {
                for col in 0..15u8 {
                    let Some(r) = ask(&[0x04, layer, row, col]) else { continue };
                    let kc = u16::from_be_bytes([r[4], r[5]]);
                    if kc == 0x7C00 || (0x5220..=0x523F).contains(&kc) {
                        let what = if kc == 0x7C00 { "QK_BOOT".to_string() } else { format!("MO({})", kc - 0x5220) };
                        println!("réteg {layer} sor {row} oszlop {col}: {what}");
                    }
                }
            }
        }
    }

    /// `cargo test rk_present -- --ignored --nocapture`
    #[test]
    #[ignore = "a csatlakoztatott hardvertől függ"]
    fn rk_present() {
        for d in Rk::default().scan() {
            println!("{}", d.tooltip());
        }
    }
}
