//! Kézzel hozzáadott eszközök (Eszközök oldal → Hozzáadás): a NapCat nem ismeri a protokolljukat,
//! ezért csak azt mutatja, hogy csatlakoztatva vannak-e (VID/PID és terméknév alapján).
//! Itt van az is, ami a Windows által észlelt, hozzáadható eszközöket felsorolja.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::hid;
use super::Provider;
use crate::model::{ChargeState, DeviceKind, DeviceState};
use crate::settings::CustomDevice;

/// A beállításokból (a tálca szál állítja be, az eszközszál olvassa).
static CUSTOM: Mutex<Vec<CustomDevice>> = Mutex::new(Vec::new());

pub fn set(list: Vec<CustomDevice>) {
    if let Ok(mut c) = CUSTOM.lock() {
        *c = list;
    }
}

pub fn kind_from_str(s: &str) -> DeviceKind {
    match s {
        "keyboard" => DeviceKind::Keyboard,
        "mouse" => DeviceKind::Mouse,
        "headset" => DeviceKind::Headset,
        "controller" => DeviceKind::Controller,
        _ => DeviceKind::Other,
    }
}

pub fn kind_to_str(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::Keyboard => "keyboard",
        DeviceKind::Mouse => "mouse",
        DeviceKind::Headset => "headset",
        DeviceKind::Controller => "controller",
        DeviceKind::Other => "other",
    }
}

#[derive(Default)]
pub struct Custom;

impl Provider for Custom {
    fn scan(&mut self) -> Vec<DeviceState> {
        let list = CUSTOM.lock().map(|c| c.clone()).unwrap_or_default();
        if list.is_empty() {
            return Vec::new();
        }
        let present = hid::interfaces();
        list.iter()
            .map(|c| {
                let connected = present.iter().any(|i| i.vid == c.vid && i.pid == c.pid && i.product == c.product);
                DeviceState {
                    connected,
                    charge: ChargeState::Unknown,
                    hw: Some((c.vid, c.pid)),
                    ..DeviceState::disconnected(&c.id(), &c.product, kind_from_str(&c.kind))
                }
            })
            .collect()
    }
}

/// Egy a Windows által észlelt, hozzáadható eszköz.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub vid: u16,
    pub pid: u16,
    pub product: String,
    pub kind: DeviceKind,
}

/// A jelenlévő HID eszközök (VID/PID + terméknév szerint összevonva), amelyeknek van billentyűzet,
/// egér, kontroller vagy headset/médiagomb kollekciója; a belső vagy rendszer-eszközök (pl. alaplapi
/// RGB vezérlő) kimaradnak.
pub fn candidates() -> Vec<Candidate> {
    let mut groups: BTreeMap<(u16, u16, String), Vec<(u16, u16)>> = BTreeMap::new();
    for i in hid::interfaces() {
        if i.vid == 0 || i.product.trim().is_empty() {
            continue;
        }
        groups.entry((i.vid, i.pid, i.product.clone())).or_default().push((i.usage_page, i.usage));
    }
    groups
        .into_iter()
        .filter_map(|((vid, pid, product), usages)| {
            let has = |page: u16, usage: u16| usages.contains(&(page, usage));
            let name = product.to_lowercase();
            let kind = if has(0x01, 0x05) || has(0x01, 0x04) {
                DeviceKind::Controller
            } else if has(0x01, 0x02) && !has(0x01, 0x06) {
                DeviceKind::Mouse
            } else if has(0x01, 0x06) {
                // A vevők gyakran billentyűzetként és egérként is jelentkeznek; a név dönt.
                if name.contains("mouse") || name.contains("egér") { DeviceKind::Mouse } else { DeviceKind::Keyboard }
            } else if has(0x0B, 0x05) || name.contains("headset") || name.contains("headphone") {
                DeviceKind::Headset
            } else if has(0x0C, 0x01) {
                DeviceKind::Other
            } else {
                return None;
            };
            Some(Candidate { vid, pid, product: product.trim().to_string(), kind })
        })
        .collect()
}
