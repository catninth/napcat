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
    Known { vid: 0x342D, pid: 0xE487, product: "R75Pro", link: Link::Cable, id: "rk-r75pro", name: "Royal Kludge R75 Pro", kind: DeviceKind::Keyboard },
    Known { vid: 0x342D, pid: 0xE487, product: "RK2.4G Dongle", link: Link::Dongle, id: "rk-r75pro", name: "Royal Kludge R75 Pro", kind: DeviceKind::Keyboard },
    Known { vid: 0x3554, pid: 0xF505, product: "VGN Dragonfly 4K Receiver", link: Link::Dongle, id: "vgn-dragonfly", name: "VGN Dragonfly", kind: DeviceKind::Mouse },
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
            out.push(DeviceState { charge, connected, ..DeviceState::disconnected(k.id, k.name, k.kind) });
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
