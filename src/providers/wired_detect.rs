//! Ismert eszközök felismerése USB-n: kábelen csatlakoztatva "tölt", a vezeték nélküli vevőn
//! (dongle) keresztül "csatlakoztatva, ismeretlen szint". Protokoll nélkül, csak a VID/PID és a
//! termék neve alapján, ezért minden kábeles-vezeték nélküli eszközre működik, amit felveszünk.

use std::collections::HashSet;

use super::hid;
use super::Provider;
use crate::model::{ChargeState, DeviceKind, DeviceState};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Link {
    Cable,
    Dongle,
}

struct Known {
    vid: u16,
    pid: u16,
    /// A HID termék neve. Kell, mert van, ahol a kábel és a dongle ugyanazt a VID/PID-et használja.
    product: &'static str,
    link: Link,
    id: &'static str,
    name: &'static str,
    kind: DeviceKind,
}

const KNOWN: &[Known] = &[
    // A G435 Lightspeed vevője akkuszintet nem ad (G HUB sem támogatja); csak Bluetooth-on ismert a szint.
    Known { vid: 0x046D, pid: 0x0ADF, product: "G435 Wireless Gaming Headset", link: Link::Dongle, id: "logi-g435", name: "Logitech G435", kind: DeviceKind::Headset },
    Known { vid: 0x046D, pid: 0x0ACB, product: "G435 Wireless Gaming Headset", link: Link::Dongle, id: "logi-g435", name: "Logitech G435", kind: DeviceKind::Headset },
];

#[derive(Default)]
pub struct WiredDetect {
    /// Az ebben a munkamenetben már látott eszközök; eltűnésük után "nincs csatlakoztatva".
    seen: HashSet<&'static str>,
}

impl Provider for WiredDetect {
    fn scan(&mut self) -> Vec<DeviceState> {
        let present: Vec<&Known> = hid::interfaces()
            .iter()
            .filter_map(|i| KNOWN.iter().find(|k| k.vid == i.vid && k.pid == i.pid && k.product == i.product))
            .collect();

        let mut out: Vec<DeviceState> = Vec::new();
        for k in KNOWN {
            if out.iter().any(|d| d.id == k.id) {
                continue;
            }
            let links: Vec<Link> = present.iter().filter(|p| p.id == k.id).map(|p| p.link).collect();
            let (connected, charge) = if links.contains(&Link::Cable) {
                (true, ChargeState::Charging)
            } else if links.contains(&Link::Dongle) {
                (true, ChargeState::Unknown)
            } else if self.seen.contains(k.id) {
                (false, ChargeState::Unknown)
            } else {
                continue;
            };
            self.seen.insert(k.id);
            out.push(DeviceState { charge, connected, hw: Some((k.vid, k.pid)), ..DeviceState::disconnected(k.id, k.name, k.kind) });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gépen most látható ismert eszközök (`cargo test scan_present -- --ignored --nocapture`).
    #[test]
    #[ignore = "a csatlakoztatott hardvertől függ"]
    fn scan_present() {
        for d in WiredDetect::default().scan() {
            println!("{}", d.tooltip());
        }
    }
}
